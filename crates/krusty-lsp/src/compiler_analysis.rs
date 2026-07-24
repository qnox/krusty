//! Compiler-facing source analysis isolated from the long-lived LSP supervisor.

mod completion;
mod semantic;

use krusty::ast::File;
use krusty::diag::{DiagSink, Diagnostic};
use krusty::frontend;
use krusty::libraries::SemanticPlatform;

#[cfg(test)]
pub(crate) use completion::CompletionKind;
pub(crate) use completion::CompletionSymbols;
pub use krusty::frontend::{FrontendSymbols, FrontendTypeInfo};
pub use semantic::{HighlightOccurrence, HighlightSymbols};

pub struct FileAnalysis {
    pub file: File,
    pub types: Option<FrontendTypeInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct SourceSetAnalysis {
    pub files: Vec<FileAnalysis>,
    pub symbols: FrontendSymbols,
}

impl FileAnalysis {
    pub fn typed_expressions(
        &self,
    ) -> impl Iterator<Item = (krusty::diag::Span, krusty::types::Ty)> + '_ {
        let types = self
            .types
            .as_ref()
            .map(|types| types.expr_types.as_slice())
            .unwrap_or(&[]);
        self.file
            .expr_spans
            .iter()
            .copied()
            .zip(types.iter().copied())
    }
}

/// Analyze a jointly compiled in-memory source set.
///
/// Sources are parsed once, signatures and inferred returns are collected globally, and every file
/// is checked in that shared context. This mirrors the batch compiler while retaining a compact
/// per-file handoff for editor queries.
pub fn analyze_source_set(
    sources: &[&str],
    platform: Box<dyn SemanticPlatform>,
) -> SourceSetAnalysis {
    let mut diags = DiagSink::new();
    let mut files = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        diags.set_file(index as u32);
        files.push(frontend::parse_source_with_detected_features(
            source, &mut diags,
        ));
    }

    let parse_errors: Vec<_> = (0..sources.len())
        .map(|index| {
            diags
                .diags
                .iter()
                .any(|diagnostic| diagnostic.file as usize == index)
        })
        .collect();
    let mut symbols = frontend::collect_signatures_with_cp(&files, platform, &mut diags);
    frontend::preinfer_module_returns(&files, &mut symbols, &mut diags);
    let types: Vec<_> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            if parse_errors[index] {
                None
            } else {
                diags.set_file(index as u32);
                Some(frontend::check_file_at(
                    file,
                    index as u32,
                    &mut symbols,
                    &mut diags,
                ))
            }
        })
        .collect();
    let mut diagnostics = vec![Vec::new(); sources.len()];
    for mut diagnostic in diags.diags {
        let file = diagnostic.file as usize;
        if let Some(file_diagnostics) = diagnostics.get_mut(file) {
            diagnostic.file = 0;
            file_diagnostics.push(diagnostic);
        }
    }
    let files = files
        .into_iter()
        .zip(types)
        .zip(diagnostics)
        .map(|((file, types), diagnostics)| FileAnalysis {
            file,
            types,
            diagnostics,
        })
        .collect();
    SourceSetAnalysis { files, symbols }
}

pub fn analyze_standalone_source_set(sources: &[&str]) -> SourceSetAnalysis {
    analyze_source_set(sources, Box::new(krusty::libraries::EmptySymbolSource))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_set_analysis_resolves_cross_file_declarations() {
        let sources = [
            "package demo\nfun answer(): Int = 42",
            "package demo\nfun use(): Int = answer()",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        assert!(
            analysis.files[1].diagnostics.is_empty(),
            "{:?}",
            analysis.files[1].diagnostics
        );
        assert!(analysis.files[1].types.is_some());
    }

    #[test]
    fn source_set_preinfers_cross_file_expression_body_returns() {
        let sources = [
            "package demo\nfun box(): Int = value()",
            "package demo\nfun value() = helper()\nfun helper() = 1",
        ];
        let analysis = analyze_standalone_source_set(&sources);
        assert!(
            !analysis.files[0]
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.msg.contains("unresolved member")),
            "{:?}",
            analysis.files[0].diagnostics
        );
        let value_offset = sources[0].rfind("value").unwrap() as u32;
        assert!(
            analysis.files[0].typed_expressions().any(|(span, ty)| {
                span.lo <= value_offset && value_offset < span.hi && ty == krusty::types::Ty::Int
            }),
            "caller was checked before value() acquired its inferred Int return"
        );
    }

    #[test]
    fn empty_source_set_is_valid_after_last_document_closes() {
        let analysis = analyze_standalone_source_set(&[]);
        assert!(analysis.files.is_empty());
    }
}
