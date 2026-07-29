//! Compiler-facing source analysis isolated from the long-lived LSP supervisor.

mod completion;
mod document_symbols;
mod folding_ranges;
pub mod java;
mod navigation;
mod rendering;
mod semantic;
mod signature_help;
mod source_scan;

use krusty::ast::{Decl, File, FunBody, FunDecl, PropDecl, Stmt};
use krusty::diag::{DiagSink, Diagnostic, DiagnosticKind, Severity};
use krusty::features::LangFeatures;
use krusty::frontend;
use krusty::libraries::SemanticPlatform;
use krusty::source::SourceInput;
use krusty::types::Ty;
use std::collections::HashMap;

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
pub use navigation::{DefinitionOccurrence, DefinitionSymbols, DefinitionTarget, LibraryRef};
pub(crate) use rendering::render_ty;
pub(crate) use semantic::{hover_wire_cost, SemanticLimits, MAX_LIBRARY_DEFINITION_BYTES};
pub use semantic::{HighlightOccurrence, HighlightSymbols, HoverOccurrence};
pub(crate) use signature_help::{SignatureCandidate, SignatureHelpCall, SignatureHelpSymbols};

const BOOLEAN_EXPRESSION_SIMPLIFICATION: &str = "Boolean expression can be simplified";
const UNUSED_EXTENSION_RECEIVER: &str = "Receiver parameter is never used";

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
#[cfg(test)]
pub fn analyze_source_set(
    sources: &[&str],
    platform: Box<dyn SemanticPlatform>,
) -> SourceSetAnalysis {
    analyze_source_set_with_features(sources, platform, &LangFeatures::new())
}

#[cfg(test)]
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
    let analysis = frontend::analyze_source_set_prefix_with_features_trimmed(
        inputs,
        checked_count,
        inferred_count,
        platform,
        project_features,
        &mut diags,
    );
    let mut diagnostics: Vec<Vec<Diagnostic>> = vec![Vec::new(); inputs.len()];
    let mut seen = vec![HashMap::<DiagnosticKey, Vec<usize>>::new(); inputs.len()];
    for mut diagnostic in diags.diags {
        let file = diagnostic.file as usize;
        if let Some(file_diagnostics) = diagnostics.get_mut(file) {
            if let Some(editor_span) = diagnostic.editor_span.take() {
                diagnostic.span = editor_span;
            }
            diagnostic.file = 0;
            let key = diagnostic_key(&diagnostic);
            if seen[file].get(&key).is_some_and(|indices| {
                indices
                    .iter()
                    .any(|&index| file_diagnostics[index].msg.as_str() == diagnostic.msg.as_str())
            }) {
                continue;
            }
            seen[file]
                .entry(key)
                .or_default()
                .push(file_diagnostics.len());
            file_diagnostics.push(diagnostic);
        }
    }
    let files = analysis
        .files
        .into_iter()
        .zip(analysis.types)
        .zip(diagnostics)
        .map(|((file, types), diagnostics)| {
            let diagnostics = with_ide_inspections(&file, types.as_ref(), diagnostics);
            FileAnalysis {
                file,
                types,
                diagnostics,
            }
        })
        .collect();
    SourceSetAnalysis {
        files,
        symbols: analysis.symbols,
    }
}

type DiagnosticKey = (u32, u32, u8, u8);

fn diagnostic_key(diagnostic: &Diagnostic) -> DiagnosticKey {
    (
        diagnostic.span.lo,
        diagnostic.span.hi,
        match diagnostic.severity {
            Severity::Error => 0,
            Severity::Warning => 1,
        },
        match diagnostic.kind {
            DiagnosticKind::Compiler => 0,
            DiagnosticKind::IncompatibleEquality => 1,
            DiagnosticKind::Inspection => 2,
        },
    )
}

fn with_ide_inspections(
    file: &File,
    types: Option<&FrontendTypeInfo>,
    diagnostics: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    let mut result = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        if diagnostic.kind == DiagnosticKind::IncompatibleEquality {
            result.push(Diagnostic {
                span: diagnostic.span,
                editor_span: None,
                identity: None,
                severity: Severity::Warning,
                kind: DiagnosticKind::Inspection,
                msg: BOOLEAN_EXPRESSION_SIMPLIFICATION.to_string(),
                file: diagnostic.file,
            });
        }
        result.push(diagnostic);
    }
    if let Some(types) = types {
        add_unused_extension_receiver_inspections(file, types, &mut result);
    }
    result
}

fn add_unused_extension_receiver_inspections(
    file: &File,
    types: &FrontendTypeInfo,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut inspections = Vec::new();
    for &declaration in &file.decls {
        match file.decl(declaration) {
            Decl::Fun(function) => {
                add_unused_extension_receiver_inspection(types, function, &mut inspections);
            }
            Decl::Class(class) => {
                for function in class.methods.iter().chain(&class.companion_methods) {
                    add_unused_extension_receiver_inspection(types, function, &mut inspections);
                }
                for entry in &class.enum_entries {
                    for function in &entry.methods {
                        add_unused_extension_receiver_inspection(types, function, &mut inspections);
                    }
                    for property in &entry.props {
                        add_unused_extension_property_inspection(types, property, &mut inspections);
                    }
                }
                for property in class.body_props.iter().chain(&class.companion_props) {
                    add_unused_extension_property_inspection(types, property, &mut inspections);
                }
            }
            Decl::Property(property) => {
                add_unused_extension_property_inspection(types, property, &mut inspections);
            }
        }
    }
    for statement in &file.stmt_arena {
        if let Stmt::LocalFun(function) = statement {
            add_unused_extension_receiver_inspection(types, function, &mut inspections);
        }
    }
    inspections.sort_by_key(|diagnostic| (diagnostic.span.lo, diagnostic.span.hi));
    diagnostics.extend(inspections);
}

fn add_unused_extension_receiver_inspection(
    types: &FrontendTypeInfo,
    function: &FunDecl,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(receiver) = function.receiver.as_ref() else {
        return;
    };
    if matches!(function.body, FunBody::None) || types.extension_receiver_is_used(receiver) {
        return;
    }
    diagnostics.push(Diagnostic {
        span: receiver.span,
        editor_span: None,
        identity: None,
        severity: Severity::Warning,
        kind: DiagnosticKind::Inspection,
        msg: UNUSED_EXTENSION_RECEIVER.to_string(),
        file: 0,
    });
}

fn add_unused_extension_property_inspection(
    types: &FrontendTypeInfo,
    property: &PropDecl,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(receiver) = property.receiver.as_ref() else {
        return;
    };
    let has_accessor_body = property.getter.is_some()
        || property
            .setter
            .as_ref()
            .is_some_and(|setter| setter.body.is_some());
    if !has_accessor_body || types.extension_receiver_is_used(receiver) {
        return;
    }
    diagnostics.push(Diagnostic {
        span: receiver.span,
        editor_span: None,
        identity: None,
        severity: Severity::Warning,
        kind: DiagnosticKind::Inspection,
        msg: UNUSED_EXTENSION_RECEIVER.to_string(),
        file: 0,
    });
}

#[cfg(test)]
pub fn analyze_standalone_source_set(sources: &[&str]) -> SourceSetAnalysis {
    analyze_source_set(sources, Box::new(krusty::libraries::EmptySymbolSource))
}

pub(crate) fn analyze_standalone_source_inputs(inputs: &[SourceInput<'_>]) -> SourceSetAnalysis {
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
    fn repeated_resolution_reports_each_diagnostic_once() {
        let source = "class C {\n    fun f(): Gone = TODO()\n    fun g(): Gone = TODO()\n}";

        let analysis = analyze_standalone_source_set(&[source]);

        let unresolved = analysis.files[0]
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.msg.contains("'Gone'"))
            .collect::<Vec<_>>();
        assert_eq!(unresolved.len(), 2, "{unresolved:?}");
        assert_ne!(unresolved[0].span.lo, unresolved[1].span.lo);
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
    fn kotlin_script_allows_unmodeled_host_calls_without_hiding_other_errors() {
        let script = "fun localAction() {}\n\
                      hostAction(\"sample\")\n\
                      hostNamed(value = if (1) \"sample\" else \"other\")\n\
                      if (1) {}\n\
                      localAction(\"sample\")";
        let support = "fun hostAction() {}";
        let analysis = analyze_standalone_source_inputs(&[
            SourceInput::new(krusty::source::SourceKind::KotlinScript, script),
            SourceInput::kotlin(support),
        ]);
        let messages = analysis.files[0]
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            messages
                .iter()
                .filter(|message| message.contains("condition type mismatch"))
                .count(),
            2,
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("localAction")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("hostAction") && !message.contains("hostNamed")),
            "{messages:?}"
        );
        assert!(analysis.files[1].diagnostics.is_empty());
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
    fn source_set_adds_unused_extension_receiver_inspections_at_receiver_types() {
        let source = "interface Parser\n\
                      interface JsonParser : Parser\n\
                      const val ANSWER = 42\n\
                      class Used(val length: Int) { fun trim(): String = \"\" }\n\
                      class Mutable(var value: Int)\n\
                      class Container(val answer: Int) {\n\
                        fun helper(): Int = 1\n\
                        fun String.dispatchPropertyOnly(): Int = answer\n\
                        fun String.dispatchCallOnly(): Int = helper()\n\
                      }\n\
                      class Collision(val length: Int) {\n\
                        fun String.extensionReceiverWins(): Int = length\n\
                        fun String.labelledDispatchOnly(): Int = this@Collision.length\n\
                      }\n\
                      fun <T, R> applyValue(value: T, block: (T) -> R): R = block(value)\n\
                      fun <T, R> T.scopeValue(block: T.() -> R): R = block()\n\
                      fun JsonParser.decode(source: String): Any = source\n\
                      fun Parser.decode(source: String): Any = source\n\
                      fun Parser.decode(value: Int): Any = value\n\
                      fun String.topLevelPropertyOnly(): Int = ANSWER\n\
                      fun String.implicitLambdaParameterOnly(): Int = applyValue(1) { it }\n\
                      fun String.nestedReceiverOnly(): Int = Used(1).run { length }\n\
                      fun String.labelledOuterUse(): Int = 1.scopeValue { this@labelledOuterUse.length }\n\
                      fun Used.explicitUse(): Int = this.length\n\
                      fun Used.implicitPropertyUse(): Int = length\n\
                      fun Used.implicitCallUse(): String = trim()\n\
                      fun Used.implicitCallableUse(): () -> String = ::trim\n\
                      fun Used.explicitCallableUse(): () -> String = this::trim\n\
                      fun Mutable.assignmentUse() { value = 1 }\n\
                      fun Mutable.incrementUse() { value++ }\n\
                      fun localExtensions() {\n\
                        fun String.localUnused(): Int = 1\n\
                        fun String.localUsed(): Int = length\n\
                      }";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostics = &analysis.files[0].diagnostics;

        assert_eq!(diagnostics.len(), 10, "{diagnostics:?}");
        assert!(diagnostics.iter().all(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.kind == DiagnosticKind::Inspection
                && diagnostic.msg == "Receiver parameter is never used"
        }));
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| {
                    &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize]
                })
                .collect::<Vec<_>>(),
            [
                "String",
                "String",
                "String",
                "JsonParser",
                "Parser",
                "Parser",
                "String",
                "String",
                "String",
                "String"
            ]
        );
    }

    #[test]
    fn member_extensions_mark_their_extension_dispatch_receiver_used() {
        let source = "class Host {\n\
                      \u{20} fun String.render(): Int = length\n\
                      \u{20} val String.rendered: Int get() = length\n\
                      \u{20} var String.rank: Int\n\
                      \u{20}   get() = length\n\
                      \u{20}   set(value) {}\n\
                      }\n\
                      fun Host.call(value: String): Int = value.render()\n\
                      fun Host.read(value: String): Int = value.rendered\n\
                      fun Host.write(value: String) { value.rank = 1 }";
        let analysis = analyze_standalone_source_set(&[source]);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn one_expression_marks_each_selected_extension_receiver_used() {
        let source = "class Host {\n\
                      \u{20} fun String.member(): Int = length\n\
                      }\n\
                      fun Host.outer(): Int {\n\
                      \u{20} fun String.local(): Int = member()\n\
                      \u{20} return \"\".local()\n\
                      }";
        let analysis = analyze_standalone_source_set(&[source]);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn member_extension_destructuring_marks_dispatch_receiver_used() {
        let source = "class Box(val value: Int)\n\
                      class Host {\n\
                      \u{20} operator fun Box.component1(): Int = value\n\
                      }\n\
                      fun Host.read(box: Box): Int {\n\
                      \u{20} val (value) = box\n\
                      \u{20} return value\n\
                      }";
        let analysis = analyze_standalone_source_set(&[source]);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn unused_extension_receiver_inspections_ignore_nested_bindings_and_cover_full_type_spans() {
        let source = "class Entry\n\
                      class Outer { class Inner<T> }\n\
                      fun <T, R> applyValue(value: T, block: (T) -> R): R = block(value)\n\
                      fun Outer.Inner<Entry>?.dottedFunction(): Int = 1\n\
                      val Outer.Inner<Entry>?.dottedProperty: Int get() = 1\n\
                      fun (() -> Unit).parenthesizedFunction(): Int = 1\n\
                      val (() -> Unit).parenthesizedProperty: Int get() = 1\n\
                      fun String.lambdaBindingOnly(): Int = applyValue(1) { it }\n\
                      fun String.catchBindingOnly(): Int = try { 1 } catch (error: Throwable) { error.hashCode() }";
        let analysis = analyze_standalone_source_set(&[source]);
        let inspections = analysis.files[0]
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == DiagnosticKind::Inspection)
            .collect::<Vec<_>>();

        assert_eq!(inspections.len(), 6, "{inspections:?}");
        assert_eq!(
            inspections
                .iter()
                .map(|diagnostic| {
                    &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize]
                })
                .collect::<Vec<_>>(),
            [
                "Outer.Inner<Entry>?",
                "Outer.Inner<Entry>?",
                "(() -> Unit)",
                "(() -> Unit)",
                "String",
                "String"
            ]
        );
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
    fn support_files_release_body_arenas_after_analysis() {
        let sources = [
            "package fixture\nfun use(): Int = inferred() + declared()",
            "package fixture\nfun inferred() = compute()\nprivate fun compute(): Int { val x = 20\n return x + 1 }",
            "package fixture\nfun declared(): Int { val y = 1\n return y }\n\
             fun host() { class Hidden(val local: Int = 9) }\n\
             class Visible(val value: Int = 7)",
        ];
        let inputs = sources.map(SourceInput::kotlin);
        let analysis = analyze_source_inputs_prefix_with_features(
            &inputs,
            1,
            2,
            Box::new(krusty::libraries::EmptySymbolSource),
            &LangFeatures::new(),
        );

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
        assert!(
            !analysis.files[0].file.expr_arena.is_empty(),
            "open documents keep their full AST"
        );
        for support in [1, 2] {
            assert!(
                analysis.files[support].file.expr_arena.is_empty()
                    && analysis.files[support].file.stmt_arena.is_empty(),
                "support file {support} must release its body arenas once analysis is done with them"
            );
            assert!(
                !analysis.files[support].file.decl_arena.is_empty(),
                "support file {support} keeps declarations for navigation"
            );
        }

        let completion = CompletionSymbols::from_source_set(&analysis.files);
        let completion_labels = analysis.files[0]
            .scoped_completion_symbols(sources[0], &completion)
            .into_iter()
            .map(|symbol| symbol.label)
            .collect::<Vec<_>>();
        assert!(completion_labels.iter().any(|label| label == "Visible"));
        assert!(!completion_labels.iter().any(|label| label == "Hidden"));

        let signatures =
            SignatureHelpSymbols::from_source_set(&sources, &analysis.files, &analysis.symbols);
        let mut signature_labels = Vec::new();
        for group in 0.. {
            let candidates = signatures.group(group);
            if candidates.is_empty() {
                break;
            }
            signature_labels.extend(candidates.iter().map(|candidate| candidate.label.as_str()));
        }
        assert!(signature_labels
            .iter()
            .any(|label| label.starts_with("Visible(") && label.contains("value: Int = 7")));
        assert!(!signature_labels
            .iter()
            .any(|label| label.starts_with("Hidden(")));
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
    fn source_set_preserves_mixed_argument_diagnostic_and_positional_argument_span() {
        let source = "fun pair(a: Int, b: Int): Int = a + b\n\
                      fun invalid(): Int = pair(b = 2, 1)";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostics = &analysis.files[0].diagnostics;
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        let diagnostic = &diagnostics[0];
        assert_eq!(
            diagnostic.msg,
            "mixing named and positional arguments is not allowed unless the order of the arguments matches the order of the parameters."
        );
        let positional = source.rfind('1').expect("positional argument") as u32;
        assert_eq!(
            diagnostic.span,
            krusty::diag::Span::new(positional, positional + 1)
        );
        assert_eq!(diagnostics[1].msg, "no value passed for parameter 'a'.");
        let callee = source.rfind("pair").expect("callee") as u32;
        assert_eq!(
            diagnostics[1].span,
            krusty::diag::Span::new(callee, callee + 4)
        );
        assert_eq!(diagnostics[1].editor_span, None);
    }

    #[test]
    fn source_set_preserves_nullable_receiver_diagnostic_and_dot_span() {
        let source =
            "fun nullableMemberCall(value: String?): String = value. /* gap */ substring(1)";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostics = &analysis.files[0].diagnostics;
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(
            diagnostics[0].msg,
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        );
        assert_eq!(
            &source[diagnostics[0].span.lo as usize..diagnostics[0].span.hi as usize],
            "."
        );
        assert_eq!(diagnostics[0].editor_span, None);
    }

    #[test]
    fn source_set_adds_official_boolean_simplification_inspection_before_equality_error() {
        let source = "fun equal(): Boolean = 1 == \"x\"\nfun unequal(): Boolean = 1 != \"x\"";
        let analysis = analyze_standalone_source_set(&[source]);
        let diagnostics = &analysis.files[0].diagnostics;
        assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");

        for pair in diagnostics.chunks_exact(2) {
            assert_eq!(pair[0].severity, Severity::Warning);
            assert_eq!(pair[0].msg, BOOLEAN_EXPRESSION_SIMPLIFICATION);
            assert_eq!(pair[0].span, pair[1].span);
            assert_eq!(pair[1].severity, Severity::Error);
        }
        assert!(diagnostics[1].msg.starts_with("operator '=='"));
        assert!(diagnostics[3].msg.starts_with("operator '!='"));
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

    #[test]
    fn source_set_resolves_inherited_java_getter_as_property() {
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
                "package p; public interface Named { String getName(); }".into(),
            ),
            (
                String::new(),
                "package p; public interface Item extends Named {}".into(),
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

        let source = "package a\nfun use(x: p.Item): String = x.name";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_narrows_safe_call_root_after_elvis_return() {
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
        let java_sources = [(
            String::new(),
            "package p; public class Clazz { public String getJavaPsi() { return \"\"; } \
             public int getSize() { return 0; } }"
                .into(),
        )];
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

        // `u?.javaPsi ?: return` proves the safe-call ROOT `u` non-null: the plain
        // `u.size` afterwards must not report a nullable-receiver error.
        let source = "package a\n\
                      fun f(u: p.Clazz?): Int {\n\
                      \u{20} val j = u?.javaPsi ?: return 0\n\
                      \u{20} println(j)\n\
                      \u{20} return u.size\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_narrows_after_else_if_return_chain() {
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

        // The asQualifiedPath shape: an `if (this is A) return …; else if (this !is B)
        // return …` head proves `this is B` for the rest of the body, so the narrowed
        // receiver flows into the local function call and the when-arm recursion.
        let source = "package a\n\
                      interface Expr\n\
                      interface Ref : Expr\n\
                      interface Qualified : Ref {\n\
                      \u{20} val receiver: Expr\n\
                      \u{20} val selector: Expr\n\
                      }\n\
                      interface SimpleName : Ref {\n\
                      \u{20} val identifier: String\n\
                      }\n\
                      fun Expr.path(): List<String>? {\n\
                      \u{20} if (this is SimpleName) {\n\
                      \u{20}\u{20} return listOf(this.identifier)\n\
                      \u{20} }\n\
                      \u{20} else if (this !is Qualified) {\n\
                      \u{20}\u{20} return null\n\
                      \u{20} }\n\
                      \u{20} var error = false\n\
                      \u{20} val list = mutableListOf<String>()\n\
                      \u{20} fun add(expr: Qualified) {\n\
                      \u{20}\u{20} val receiver = expr.receiver\n\
                      \u{20}\u{20} val selector = expr.selector as? SimpleName ?: run { error = true; return }\n\
                      \u{20}\u{20} when (receiver) {\n\
                      \u{20}\u{20}\u{20} is Qualified -> add(receiver)\n\
                      \u{20}\u{20}\u{20} is SimpleName -> list += receiver.identifier\n\
                      \u{20}\u{20}\u{20} else -> {\n\
                      \u{20}\u{20}\u{20}\u{20} error = true\n\
                      \u{20}\u{20}\u{20}\u{20} return\n\
                      \u{20}\u{20}\u{20} }\n\
                      \u{20}\u{20} }\n\
                      \u{20}\u{20} list += selector.identifier\n\
                      \u{20} }\n\
                      \u{20} add(this)\n\
                      \u{20} return if (error) null else list\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_resolves_interface_nested_class_static_call() {
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
                "package p; public class Notification { public Notification(String a) {} }".into(),
            ),
            (
                String::new(),
                // `Bus` carries no explicit modifier: interface members are implicitly public.
                "package p; public interface Notifications {\n\
                 \u{20} final class Bus {\n\
                 \u{20}\u{20} public static void notify(Notification n) {}\n\
                 \u{20} }\n\
                 }"
                .into(),
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
                      import p.Notification\n\
                      import p.Notifications\n\
                      fun go() {\n\
                      \u{20} Notifications.Bus.notify(Notification(\"x\"))\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_binds_generic_return_from_generic_static_field() {
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
            (String::new(), "package p; public class Key<T> {}".into()),
            (
                String::new(),
                // The field's descriptor erases to raw `Key`; only its `Signature` carries
                // `Key<String>`, which the call below needs to bind `T`.
                "package p; public class Keys { public static final Key<String> NAME = null; }"
                    .into(),
            ),
            (
                String::new(),
                "package p; public class Ctx { public <T> T get(Key<T> key) { return null; } }"
                    .into(),
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
                      fun use(c: p.Ctx): Int {\n\
                      \u{20} val v = c.get(p.Keys.NAME) ?: return 0\n\
                      \u{20} return v.length\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_passes_module_subclass_to_java_member_parameter() {
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
                "package p; public abstract class Visitor {}".into(),
            ),
            (
                String::new(),
                "package p; public interface File { void accept(Visitor v); }".into(),
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

        // The DumpUastTreeActionByEach shape: an anonymous object extending a DECLARATION-ONLY
        // Kotlin class whose own base is the Java parameter type — the argument reaches
        // `accept(Visitor)` only through the module-side supertype walk.
        let main = "package a\n\
                    import p.File\n\
                    import dep.Printing\n\
                    fun go(file: File): String = object : Printing({ true }) {\n\
                    \u{20} override fun render(s: String): CharSequence? = s\n\
                    }.also { file.accept(it) }.result";
        let dep = "package dep\n\
                   import p.Visitor\n\
                   abstract class Printing(val filter: (String) -> Boolean) : Visitor() {\n\
                   \u{20} val result: String = \"\"\n\
                   \u{20} abstract fun render(s: String): CharSequence?\n\
                   }";
        let inputs = [
            krusty::source::SourceInput::kotlin(main),
            krusty::source::SourceInput::kotlin(dep),
        ];
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let mut diags = krusty::diag::DiagSink::new();
        krusty::frontend::analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            platform,
            &krusty::features::LangFeatures::new(),
            &mut diags,
        );

        let file0: Vec<_> = diags.diags.iter().filter(|d| d.file == 0).collect();
        assert!(file0.is_empty(), "{file0:?}");
    }

    #[test]
    fn source_set_passes_module_subclass_to_java_constructor_parameter() {
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
                "package p; public abstract class Visitor {}".into(),
            ),
            (
                String::new(),
                "package p; public class Holder { public Holder(Visitor v) {} }".into(),
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

        // The constructor twin of the member-argument case: the argument's path to the Java
        // parameter type runs through a MODULE-declared subclass, so `Holder(V())` resolves only
        // if constructor applicability admits the module-side supertype walk.
        let source = "package a\n\
                      import p.Holder\n\
                      import p.Visitor\n\
                      class V : Visitor()\n\
                      fun go(): Holder = Holder(V())";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_spreads_kotlin_vararg_into_java_vararg_member() {
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
            (String::new(), "package p; public interface Fix {}".into()),
            (
                String::new(),
                "package p; public interface Fix {}".into(),
            ),
            (
                String::new(),
                "package p; public class Holder {\n\
                 \u{20} public void reg(String s, Fix... fixes) {}\n\
                 }"
                .into(),
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

        // The registerUProblem shape: a Kotlin vararg forwarded with a spread into a Java
        // vararg member, plus an element-style call — both need the stub's ACC_VARARGS.
        let source = "package a\n\
                      import p.Fix\n\
                      import p.Holder\n\
                      fun Holder.forward(s: String, vararg fixes: Fix) {\n\
                      \u{20} reg(s, *fixes)\n\
                      }\n\
                      fun direct(h: Holder, f: Fix) {\n\
                      \u{20} h.reg(\"x\", f)\n\
                      \u{20} h.reg(\"y\")\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_converts_sam_lambdas_on_implicit_receivers_and_nested_interfaces() {
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
                "package p; public interface Listener { void actionPerformed(Event e); }".into(),
            ),
            (String::new(), "package p; public class Event {}".into()),
            (String::new(), "package p; public class Place {}".into()),
            (String::new(), "package p; public class Dep {}".into()),
            (
                String::new(),
                "package p; public class Button {\n\
                 \u{20} public void addActionListener(Listener l) {}\n\
                 }"
                .into(),
            ),
            (
                String::new(),
                // `Proc` is a MEMBER type referenced from its own class body — enclosing-chain
                // resolution, not package scope — and the SAM of a static trailing lambda.
                "package p; public class Builder {\n\
                 \u{20} public static void analyze(String f, Proc p) {}\n\
                 \u{20} public static void analyze(String f, Proc p, Opts o) {}\n\
                 \u{20} public interface Proc { void process(Place a, Dep b); }\n\
                 \u{20} public static class Opts {}\n\
                 }"
                .into(),
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

        // Three SAM-conversion shapes from intellij-community:
        // a zero-parameter lambda on an IMPLICIT receiver (inside .apply {}), an explicit call,
        // and a static member whose SAM is a nested interface.
        let source = "package a\n\
                      import p.Builder\n\
                      import p.Button\n\
                      fun direct(b: Button) {\n\
                      \u{20} b.addActionListener { e -> println(e) }\n\
                      }\n\
                      fun inApply(): Button = Button().apply {\n\
                      \u{20} addActionListener {\n\
                      \u{20}\u{20} println(this)\n\
                      \u{20} }\n\
                      }\n\
                      fun stat() {\n\
                      \u{20} Builder.analyze(\"f\") { place, dep -> println(place) }\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_binds_explicit_type_args_on_generic_static_sam_call() {
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
        let java_sources = [(
            String::new(),
            "package p; import java.util.Map; import java.util.function.Function;\n\
             public final class Maps {\n\
             \u{20} public static <K, V> Map<K, V> create(Function<? super K, ? extends V> f) { return null; }\n\
             }"
            .into(),
        )];
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

        // The FactoryMap.create<PsiFile, Array<DependencyRule>> shape: explicit call type
        // arguments must bind K/V so the lambda parameter types and the returned Map do —
        // without them `s` erases to Any and `m["x"]` to Any.
        let source = "package a\n\
                      import p.Maps\n\
                      fun go(): Int {\n\
                      \u{20} val m = Maps.create<String, Int> { s -> s.length }\n\
                      \u{20} return m[\"x\"] ?: 0\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_maps_all_caps_java_getters_to_properties() {
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
        let java_sources = [(
            String::new(),
            "package p; public class Language {\n\
             \u{20} public String getID() { return null; }\n\
             \u{20} public String getURLPath() { return null; }\n\
             }"
            .into(),
        )];
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

        // Kotlin's decapitalize-smart getter mapping: `getID()` reads as `id`,
        // `getURLPath()` as `urlPath` (the `language.id` shape across intellij-community).
        let source = "package a\n\
                      fun use(l: p.Language): String = l.id + l.urlPath";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        assert!(
            analysis.files[0].diagnostics.is_empty(),
            "{:?}",
            analysis.files[0].diagnostics
        );
    }

    #[test]
    fn source_set_resolves_java_setter_backed_property_write() {
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
        let java_sources = [(
            String::new(),
            "package p; public class Presentation {\n\
             \u{20} public boolean isEnabledAndVisible() { return false; }\n\
             \u{20} public void setEnabledAndVisible(boolean v) {}\n\
             \u{20} public String getText() { return null; }\n\
             \u{20} public void setText(String t) {}\n\
             \u{20} public int getRank() { return 0; }\n\
             }"
            .into(),
        )];
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

        // `isX`/`setX` and `getX`/`setX` pairs are writable synthetic properties; a getter-only
        // `rank` stays read-only ('val' cannot be reassigned).
        let source = "package a\n\
                      fun use(x: p.Presentation) {\n\
                      \u{20} x.isEnabledAndVisible = true\n\
                      \u{20} x.text = \"t\"\n\
                      }\n\
                      fun bad(x: p.Presentation) {\n\
                      \u{20} x.rank = 1\n\
                      }";
        let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
        let analysis = analyze_source_set(&[source], platform);

        let messages: Vec<&str> = analysis.files[0]
            .diagnostics
            .iter()
            .map(|d| d.msg.as_str())
            .collect();
        assert_eq!(
            messages,
            vec!["'val' cannot be reassigned."],
            "writable pairs must check clean; getter-only stays val"
        );
    }
}
