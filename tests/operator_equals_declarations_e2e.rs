//! `FirOperatorModifierChecker` for `operator fun equals`: applicability comes from the selected
//! override of `kotlin.Any.equals`, while enum-entry and anonymous-object failures use the
//! release-owned deprecation policy.

use super::common;
use krusty::compilation_target::CompilationTarget;
use krusty::kotlin_version::KotlinVersion;
use krusty::language_version::LanguageVersion;

const FORBID: &str = "-XXLanguage:+ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects";
const NOT_OVERRIDING: &str = "'operator' modifier is not applicable to function: must override \
                              'equals()' in Any.";
const NOT_MEMBER: &str =
    "'operator' modifier is not applicable to function: must be a member function.";

fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[&str], warnings: &[&str]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("OperatorEquals.kt", source)],
        &[common::stdlib_jar()],
        &args,
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let krusty_warnings = common::compiler_warnings(&krusty_output);
    let kotlinc_warnings = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(render(&kotlinc_errors), errors);
    assert_eq!(render(&kotlinc_warnings), warnings);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const DECLARATIONS: &str = "\
class C {
    operator fun equals(other: Int): Boolean = true
    override operator fun equals(other: Any?): Boolean = true
}
object O {
    operator fun equals(other: Int): Boolean = true
}
operator fun equals(other: Any?): Boolean = true
operator fun String.equals(other: Any?): Boolean = true
fun local() {
    operator fun equals(other: Any?): Boolean = true
}
interface I {
    operator fun equals(other: String): Boolean
}
";

#[test]
fn operator_equals_requires_the_resolved_any_relation() {
    assert_matches_kotlinc(
        DECLARATIONS,
        &[],
        &[
            &format!("2:5: {NOT_OVERRIDING}"),
            &format!("6:5: {NOT_OVERRIDING}"),
            &format!("8:1: {NOT_MEMBER}"),
            &format!("9:1: {NOT_MEMBER}"),
            &format!("11:5: {NOT_MEMBER}"),
            &format!("14:5: {NOT_OVERRIDING}"),
        ],
        &[],
    );
}

const RESOLVED_RELATION_FAILURES: &str = "\
class NoOverride {
    operator fun equals(other: Any?): Boolean = true
}
class WrongResult {
    override operator fun equals(other: Any?): Int = 0
}
class SuspendMismatch {
    override suspend operator fun equals(other: Any?): Boolean = true
}
";

#[test]
fn any_relation_precedes_override_result_and_suspend_diagnostics() {
    assert_matches_kotlinc(
        RESOLVED_RELATION_FAILURES,
        &[],
        &[
            "2:18: 'equals' hides member of supertype 'Any' and needs an 'override' modifier.",
            "5:48: return type of 'fun equals(other: Any?): Int' is not a subtype of the return \
             type of the overridden member 'fun equals(other: Any?): Boolean' defined in \
             'kotlin.Any'.",
            "8:35: suspend function 'equals' cannot override non-suspend function 'fun \
             equals(other: Any?): Boolean' defined in 'kotlin.Any'.",
        ],
        &[],
    );
}

const ANONYMOUS: &str = "\
enum class E {
    A {
        operator fun equals(other: Int): Boolean = true
    },
    B;
}
fun test(): Any {
    val a = object {
        operator fun equals(other: Int): Boolean = true
        override operator fun equals(other: Any?): Boolean = true
    }
    return a
}
";

#[test]
fn anonymous_owner_warning_matches_kotlinc() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let warning = format!(
        "{NOT_OVERRIDING} This will become an error in language version 2.5. See \
         https://youtrack.jetbrains.com/issue/KT-86143."
    );
    assert_matches_kotlinc(
        ANONYMOUS,
        &[],
        &[],
        &[&format!("3:9: {warning}"), &format!("9:9: {warning}")],
    );
}

#[test]
fn forbidden_anonymous_owner_failure_is_an_error() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        ANONYMOUS,
        &[FORBID],
        &[
            &format!("3:9: {NOT_OVERRIDING}"),
            &format!("9:9: {NOT_OVERRIDING}"),
        ],
        &[],
    );
}

fn diagnostics_for_release(source: &str, release: KotlinVersion) -> Vec<krusty::diag::Diagnostic> {
    let features = krusty::features::LangFeatures::for_release(
        release,
        LanguageVersion::V2_4,
        LanguageVersion::V2_4,
    );
    common::front_end_diagnostic_records_with_features_for(
        CompilationTarget::Jvm,
        source,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
        &features,
    )
}

#[test]
fn anonymous_owner_policy_is_owned_by_the_selected_release() {
    const SOURCE: &str = "fun make() = object { operator fun equals(other: Int) = true }\n";
    for release in [KotlinVersion::V2_4_0, KotlinVersion::V2_4_10] {
        let diagnostics = diagnostics_for_release(SOURCE, release);
        assert_eq!(diagnostics.len(), 1, "{release}: {diagnostics:?}");
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.span, krusty::diag::Span::new(22, 30));
        assert_eq!(diagnostic.editor_span, None);
        assert_eq!(diagnostic.severity, krusty::diag::Severity::Error);
        assert_eq!(diagnostic.name, None);
        assert_eq!(diagnostic.kind, krusty::diag::DiagnosticKind::Compiler);
        assert_eq!(diagnostic.msg, NOT_OVERRIDING);
        assert_eq!(diagnostic.identity, None);
        assert_eq!(diagnostic.file, 0);
    }

    let diagnostics = diagnostics_for_release(SOURCE, KotlinVersion::V2_4_20);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.span, krusty::diag::Span::new(22, 30));
    assert_eq!(diagnostic.editor_span, None);
    assert_eq!(diagnostic.severity, krusty::diag::Severity::Warning);
    assert_eq!(
        diagnostic.name,
        Some(krusty::diag::names::INAPPLICABLE_OPERATOR_MODIFIER_WARNING)
    );
    assert_eq!(diagnostic.kind, krusty::diag::DiagnosticKind::Compiler);
    assert_eq!(
        diagnostic.msg,
        "'operator' modifier is not applicable to function: must override 'equals()' in Any. \
         This will become an error in language version 2.5. See \
         https://youtrack.jetbrains.com/issue/KT-86143."
    );
    assert_eq!(diagnostic.identity, None);
    assert_eq!(diagnostic.file, 0);
}

#[test]
fn suppression_hides_only_the_warning_phase() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let source = "@Suppress(\"INAPPLICABLE_OPERATOR_MODIFIER_WARNING\")\n\
                  fun make() = object { operator fun equals(other: Int) = true }\n";
    assert_matches_kotlinc(source, &[], &[], &[]);
    assert_matches_kotlinc(
        source,
        &[FORBID],
        &[&format!("2:23: {NOT_OVERRIDING}")],
        &[],
    );
}

#[test]
fn named_warning_policy_controls_the_exact_operator_diagnostic() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let source = "fun make() = object {\n\
                      operator fun equals(other: Int): Boolean = true\n\
                  }\n";
    assert_matches_kotlinc(
        source,
        &["-Xwarning-level=INAPPLICABLE_OPERATOR_MODIFIER_WARNING:disabled"],
        &[],
        &[],
    );
    let warning = format!(
        "{NOT_OVERRIDING} This will become an error in language version 2.5. See \
         https://youtrack.jetbrains.com/issue/KT-86143."
    );
    assert_matches_kotlinc(
        source,
        &["-Xwarning-level=INAPPLICABLE_OPERATOR_MODIFIER_WARNING:error"],
        &[&format!("2:1: {warning}")],
        &[],
    );
}

#[test]
fn custom_typed_equals_uses_the_selected_value_class_representation() {
    let inline = "@JvmInline value class Inline(val x: Int) {\n\
                      operator fun equals(other: Inline): Boolean = x == other.x\n\
                  }\n\
                  @JvmInline value class Bottom(val x: Int) {\n\
                      operator fun equals(other: Bottom): Nothing = throw IllegalStateException()\n\
                  }\n";
    assert_matches_kotlinc(
        inline,
        &["-XXLanguage:+CustomEqualsInValueClasses"],
        &[],
        &[],
    );

    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let full = "value class Full(val x: Int) {\n\
                    operator fun equals(other: Full): Boolean = x == other.x\n\
                }\n\
                value class Generic<T>(val x: T) {\n\
                    operator fun equals(other: Generic<T>): Int = 0\n\
                }\n";
    assert_matches_kotlinc(
        full,
        &[
            "-XXLanguage:+FullValueClasses",
            "-XXLanguage:+CustomEqualsInValueClasses",
        ],
        &[
            "2:1: 'operator' modifier is not applicable to function: must override 'equals()' in \
           Any or define 'equals(other: Full): Boolean'.",
            "5:1: 'operator' modifier is not applicable to function: must override 'equals()' in \
             Any or define 'equals(other: Generic<*>): Boolean'.",
        ],
        &[],
    );
}
