//! Compiler-facing source analysis isolated from the long-lived LSP supervisor.

mod completion;
mod document_symbols;
mod folding_ranges;
mod navigation;
mod rendering;
mod semantic;
mod signature_help;
mod source_scan;

use krusty::ast::{File, FunBody, PropDecl};
use krusty::diag::{DiagSink, Diagnostic};
use krusty::features::LangFeatures;
use krusty::frontend;
use krusty::libraries::SemanticPlatform;
use krusty::source::SourceInput;
use krusty::types::Ty;

pub(crate) use completion::{CompletionDetails, CompletionKind, CompletionSymbols};
pub(crate) use document_symbols::{document_symbol_occurrences, DocumentSymbolOccurrence};
#[cfg(test)]
pub(crate) use folding_ranges::FoldingRangeText;
pub(crate) use folding_ranges::{
    folding_range_occurrences, FoldingRangeOccurrence, FOLDING_KIND_COMMENT, FOLDING_KIND_IMPORTS,
    FOLDING_KIND_REGION, TEXT_BLOCK_COMMENT, TEXT_BRACES, TEXT_IMPORTS, TEXT_KDOC,
    TEXT_PARENTHESES, TEXT_RAW_STRING, TEXT_REGION_LABEL,
};
pub use krusty::frontend::{FrontendSymbols, FrontendTypeInfo};
pub use navigation::{DefinitionOccurrence, DefinitionSymbols, DefinitionTarget};
pub(crate) use semantic::{hover_wire_cost, SemanticLimits};
pub use semantic::{HighlightOccurrence, HighlightSymbols, HoverOccurrence};
pub(crate) use signature_help::{SignatureCandidate, SignatureHelpCall, SignatureHelpSymbols};

pub struct FileAnalysis {
    pub file: File,
    pub types: Option<FrontendTypeInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct SourceSetAnalysis {
    pub files: Vec<FileAnalysis>,
    pub symbols: FrontendSymbols,
}

fn checked_property_type(
    property: &PropDecl,
    types: Option<&FrontendTypeInfo>,
    resolved_type: Option<Ty>,
) -> Option<Ty> {
    resolved_type.or_else(|| {
        property
            .init
            .or_else(|| {
                property.getter.as_ref().and_then(|getter| match getter {
                    FunBody::Expr(body) | FunBody::Block(body) => Some(*body),
                    FunBody::None => None,
                })
            })
            .or(property.delegate)
            .and_then(|expression| {
                let types = types?;
                types
                    .delegate_getvalue(expression)
                    .map(|target| target.ret())
                    .or_else(|| types.expr_types.get(expression.0 as usize).copied())
            })
    })
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
    analyze_source_set_with_features(sources, platform, &LangFeatures::new())
}

pub fn analyze_source_set_with_features(
    sources: &[&str],
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
) -> SourceSetAnalysis {
    let inputs = sources
        .iter()
        .map(|source| SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    analyze_source_inputs_with_features(&inputs, platform, project_features)
}

pub fn analyze_source_inputs_with_features(
    inputs: &[SourceInput<'_>],
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
) -> SourceSetAnalysis {
    let mut diags = DiagSink::new();
    let analysis =
        frontend::analyze_source_set_with_features(inputs, platform, project_features, &mut diags);
    let mut diagnostics = vec![Vec::new(); inputs.len()];
    for mut diagnostic in diags.diags {
        let file = diagnostic.file as usize;
        if let Some(file_diagnostics) = diagnostics.get_mut(file) {
            if let Some(editor_span) = diagnostic.editor_span.take() {
                diagnostic.span = editor_span;
            }
            diagnostic.file = 0;
            file_diagnostics.push(diagnostic);
        }
    }
    let files = analysis
        .files
        .into_iter()
        .zip(analysis.types)
        .zip(diagnostics)
        .map(|((file, types), diagnostics)| FileAnalysis {
            file,
            types,
            diagnostics,
        })
        .collect();
    SourceSetAnalysis {
        files,
        symbols: analysis.symbols,
    }
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
    fn source_set_uses_editor_diagnostic_span_without_retaining_both_ranges() {
        let source = "fun pair(left: Int, right: String): Int = left\n\
                      fun missing(): Int = pair(1)";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostic = analysis.files[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.msg == "no value passed for parameter 'right'.")
            .expect("missing-argument diagnostic");
        assert_eq!(
            &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
            "pair"
        );
        assert_eq!(
            diagnostic.editor_span, None,
            "the LSP handoff must consume the alternate span instead of retaining duplicate metadata"
        );
    }

    #[test]
    fn source_set_preserves_unknown_named_argument_diagnostic_and_exact_name_span() {
        let source = "fun pair(left: Int, right: String): String = right\n\
                      fun invalid(): String = pair(left = 1, unknown = 2, right = \"ok\")";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostic = analysis.files[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.msg == "no parameter with name 'unknown' found.")
            .expect("unknown named-argument diagnostic");
        assert_eq!(
            &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
            "unknown"
        );
    }

    #[test]
    fn empty_source_set_is_valid_after_last_document_closes() {
        let analysis = analyze_standalone_source_set(&[]);
        assert!(analysis.files.is_empty());
    }
}
