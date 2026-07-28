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
use krusty::diag::{DiagSink, Diagnostic, DiagnosticKind, Severity};
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

const BOOLEAN_EXPRESSION_SIMPLIFICATION: &str = "Boolean expression can be simplified";

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
    analyze_source_inputs_prefix_with_features(
        inputs,
        inputs.len(),
        inputs.len(),
        platform,
        project_features,
    )
}

pub fn analyze_source_inputs_prefix_with_features(
    inputs: &[SourceInput<'_>],
    checked_count: usize,
    inferred_count: usize,
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
) -> SourceSetAnalysis {
    let mut diags = DiagSink::new();
    let analysis = frontend::analyze_source_set_prefix_with_features(
        inputs,
        checked_count,
        inferred_count,
        platform,
        project_features,
        &mut diags,
    );
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
            diagnostics: with_ide_inspections(diagnostics),
        })
        .collect();
    SourceSetAnalysis {
        files,
        symbols: analysis.symbols,
    }
}

fn with_ide_inspections(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut result = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        if diagnostic.kind == DiagnosticKind::IncompatibleEquality {
            result.push(Diagnostic {
                span: diagnostic.span,
                editor_span: None,
                severity: Severity::Warning,
                kind: DiagnosticKind::Inspection,
                msg: BOOLEAN_EXPRESSION_SIMPLIFICATION.to_string(),
                file: diagnostic.file,
            });
        }
        result.push(diagnostic);
    }
    result
}

pub fn analyze_standalone_source_set(sources: &[&str]) -> SourceSetAnalysis {
    analyze_source_set(sources, Box::new(krusty::libraries::EmptySymbolSource))
}

#[cfg(test)]
fn analyze_standalone_source_inputs(inputs: &[SourceInput<'_>]) -> SourceSetAnalysis {
    analyze_source_inputs_with_features(
        inputs,
        Box::new(krusty::libraries::EmptySymbolSource),
        &LangFeatures::new(),
    )
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
    fn kotlin_script_accepts_and_checks_a_top_level_call() {
        let source = "fun render(value: String): String = value\n\
                      fun suspend(block: () -> Unit) = block()\n\
                      fun context(value: String): String = value\n\
                      render(\"sample\")\n\
                      context(\"sample\")\n\
                      fun after_line_call(): Unit {}\n\
                      context(\"sample\"); fun after_semicolon_call(): Unit {}\n\
                      suspend {}";
        let inputs = [SourceInput::new(
            krusty::source::SourceKind::KotlinScript,
            source,
        )];

        let analysis = analyze_standalone_source_inputs(&inputs);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
        assert!(analysis.files[0].file.script_body.is_some());
        let call_start = source.find("render(\"sample\")").unwrap() as u32;
        assert!(analysis.files[0]
            .typed_expressions()
            .any(|(span, ty)| span.lo == call_start && ty == Ty::String));

        let kotlin = analyze_standalone_source_inputs(&[SourceInput::kotlin(source)]);
        assert!(kotlin.files[0]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.msg == "expected a top-level declaration"));
    }

    #[test]
    fn source_set_adds_equality_inspections_before_compiler_errors() {
        let source = "fun equal(): Boolean = 1 == \"text\"\nfun unequal(): Boolean = 1 != \"text\"";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostics = &analysis.files[0].diagnostics;
        assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");

        for pair in diagnostics.chunks_exact(2) {
            assert_eq!(pair[0].severity, Severity::Warning);
            assert_eq!(pair[0].kind, DiagnosticKind::Inspection);
            assert_eq!(pair[0].msg, BOOLEAN_EXPRESSION_SIMPLIFICATION);
            assert_eq!(pair[0].span, pair[1].span);
            assert_eq!(pair[1].kind, DiagnosticKind::IncompatibleEquality);
        }
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
    fn prefix_analysis_resolves_a_support_source_companion_declaration() {
        let inputs = [
            SourceInput::kotlin(
                "package feature\nimport fixture.Bridge\n\
                 fun use(): Bridge? {\n\
                 \u{20} val bridge = Bridge.current()\n\
                 \u{20} if (bridge == null) return null\n\
                 \u{20} return bridge.next()\n\
                 }",
            ),
            SourceInput::kotlin(
                "package fixture\ninterface Bridge {\n\
                 \u{20} fun next(): Bridge?\n\
                 \u{20} companion object { fun current(): Bridge? = null }\n\
                 }",
            ),
        ];
        let analysis = analyze_source_inputs_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(krusty::libraries::EmptySymbolSource),
            &LangFeatures::new(),
        );

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
        assert!(analysis.files[0].types.is_some());
        assert!(
            analysis.files[1].types.is_none(),
            "support declarations must not diagnose dependency bodies as part of the consumer module"
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
    fn source_set_preserves_duplicate_named_argument_diagnostic_and_second_name_span() {
        let source = "fun pair(a: Int, b: String): String = b\n\
                      fun invalid(): String = pair(a = 1, a = 2, b = \"ok\")";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostic = analysis.files[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.msg == "argument already passed for this parameter.")
            .expect("duplicate named-argument diagnostic");
        let duplicate = source.rfind("a = 2").expect("duplicate named argument") as u32;
        assert_eq!(
            diagnostic.span,
            krusty::diag::Span::new(duplicate, duplicate + 1)
        );
    }

    #[test]
    fn empty_source_set_is_valid_after_last_document_closes() {
        let analysis = analyze_standalone_source_set(&[]);
        assert!(analysis.files.is_empty());
    }

    #[test]
    fn source_set_resolves_unbuilt_java_declarations() {
        let Some(stdlib) = krusty::toolchain::stdlib_jar() else {
            return;
        };
        let Some(jdk_modules) = krusty::toolchain::jdk_modules() else {
            return;
        };
        let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
            stdlib,
            jdk_modules,
        ]));
        classpath.prepare_for_source_analysis();
        let java_sources = [
            (
                String::new(),
                "package p; public class Widget { public int size() { return 0; } }".into(),
            ),
            (
                String::new(),
                "package p; public enum Color { RED, GREEN }".into(),
            ),
            (
                String::new(),
                "package p; public record Pt(int x) {}".into(),
            ),
            (
                String::new(),
                "package p; public @interface Tag { int value(); }".into(),
            ),
        ];
        let stubs = krusty::jvm::java_stub::stub_classes(
            &java_sources,
            krusty::jvm::java_stub::StubMode::Lenient,
            &|candidate| {
                classpath
                    .find_name(krusty::types::type_name(candidate))
                    .is_some()
            },
        )
        .expect("Java stubs");
        classpath.set_stub_overlay(stubs);

        let source = "package a\n\
                      @p.Tag(1)\n\
                      fun use(w: p.Widget, c: p.Color, pt: p.Pt): Int {\n\
                      \u{20} val n = if (c == p.Color.RED) 1 else 2\n\
                      \u{20} return w.size() + pt.x() + n\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }
}
