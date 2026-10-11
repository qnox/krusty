//! kotlinc's `operator fun equals` declaration checks (`FirOperatorModifierChecker`): the modifier
//! applies only to a member that overrides `Any.equals`. In an enum entry or an anonymous object a
//! non-overriding `equals` is a deprecation warning until
//! `ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects` makes it the error.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidOperatorEqualsInEnumEntriesAndAnonymousObjects";

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`.
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

const NOT_OVERRIDING: &str = "'operator' modifier is not applicable to function: must override \
                              'equals()' in Any.";
const NOT_MEMBER: &str =
    "'operator' modifier is not applicable to function: must be a member function.";

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
fn operator_equals_must_be_a_member_overriding_any_equals() {
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
fn operator_equals_in_anonymous_objects_is_a_deprecation_warning() {
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
fn forbidden_operator_equals_in_anonymous_objects_is_an_error() {
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
