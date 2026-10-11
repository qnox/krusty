//! kotlinc's `FirClassLiteralChecker` for a class literal whose left-hand side is written with
//! type arguments that select nothing: a reified type parameter, or a typealias. The useless forms
//! keep a deprecation warning until `ForbidUselessTypeArgumentsIn25`; any other typealias is always
//! the error.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidUselessTypeArgumentsIn25";
const DEPRECATION: &str =
    "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.";
const NOT_A_CLASS: &str = "only classes are allowed on the left-hand side of a class literal.";
const TYPE_PARAMETER: &str = "type arguments are not allowed for type parameters.";

/// Compile `sources` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`.
fn assert_matches_kotlinc(
    sources: &[(&str, &str)],
    args: &[&str],
    errors: &[String],
    warnings: &[String],
) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result =
        common::compiler_diagnostics_with_shared_args(sources, &[common::stdlib_jar()], &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}:{}: {}",
                    diagnostic.file, diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    let kotlinc_errors = render(&common::compiler_errors(&result.reference_stderr));
    let kotlinc_warnings = render(&common::compiler_warnings(&result.reference_stderr));
    assert_eq!(
        kotlinc_errors, errors,
        "kotlinc\n{}",
        result.reference_stderr
    );
    assert_eq!(
        kotlinc_warnings, warnings,
        "kotlinc\n{}",
        result.reference_stderr
    );
    assert_eq!(
        render(&common::compiler_errors(&krusty_output)),
        kotlinc_errors,
        "errors\n{krusty_output}"
    );
    assert_eq!(
        render(&common::compiler_warnings(&krusty_output)),
        kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const LITERALS: &str = "\
typealias No2 = String
typealias Yes2<T> = No2
typealias No3 = Array<String>
typealias Yes3<T> = No3
typealias Str = String
typealias Arr<T> = Array<T>
typealias Lst<T> = List<T>

object OutObject {
    object InObject
}
typealias OutObjectInObject = OutObject.InObject
typealias OutObjectInObjectWithT<T> = OutObject.InObject

inline fun <reified T> reified() = T<Int>::class
inline fun <reified T> reified2() = T<Int, String>::class
inline fun <reified T> plain() = T::class

val a = Yes2<Int>::class
val b = Yes3<Int>::class
val c = Yes2<Int, String>::class
val d = Yes3<Int, String>::class
val e = Str<Int>::class
val f = OutObjectInObject<Any>::class
val g = OutObjectInObjectWithT<Any>::class
val h = Array<Int>::class
val i = Arr<Int>::class
val j = Arr<Int, Int>::class

@Suppress(\"WRONG_NUMBER_OF_TYPE_ARGUMENTS_IN_GET_CLASS_WARNING\")
val s = Str<Int>::class
";

/// Every diagnostic kotlinc reports on [`LITERALS`] without the `@Suppress`ed one, in order.
fn literal_diagnostics() -> Vec<(&'static str, String)> {
    vec![
        ("Literals.kt:15:37", TYPE_PARAMETER.to_string()),
        ("Literals.kt:16:38", TYPE_PARAMETER.to_string()),
        ("Literals.kt:19:9", NOT_A_CLASS.to_string()),
        ("Literals.kt:20:9", NOT_A_CLASS.to_string()),
        (
            "Literals.kt:21:13",
            "one type argument expected for 'typealias Yes2<T> = String'.".to_string(),
        ),
        (
            "Literals.kt:22:13",
            "one type argument expected for 'typealias Yes3<T> = Array<String>'.".to_string(),
        ),
        (
            "Literals.kt:23:12",
            "no type arguments expected for 'typealias Str = String'.".to_string(),
        ),
        (
            "Literals.kt:24:26",
            "no type arguments expected for 'typealias OutObjectInObject = OutObject.InObject'."
                .to_string(),
        ),
        ("Literals.kt:25:9", NOT_A_CLASS.to_string()),
        ("Literals.kt:27:9", NOT_A_CLASS.to_string()),
        (
            "Literals.kt:28:12",
            "one type argument expected for 'typealias Arr<T> = Array<T>'.".to_string(),
        ),
    ]
}

#[test]
fn useless_class_literal_type_arguments_are_deprecated() {
    let warnings = literal_diagnostics()
        .into_iter()
        .map(|(location, message)| format!("{location}: {message} {DEPRECATION}"))
        .collect::<Vec<_>>();
    assert_matches_kotlinc(&[("Literals.kt", LITERALS)], &[], &[], &warnings);
}

#[test]
fn forbidden_useless_class_literal_type_arguments_are_errors() {
    let mut errors = literal_diagnostics()
        .into_iter()
        .map(|(location, message)| format!("{location}: {message}"))
        .collect::<Vec<_>>();
    errors.push(
        "Literals.kt:31:12: no type arguments expected for 'typealias Str = String'.".to_string(),
    );
    assert_matches_kotlinc(&[("Literals.kt", LITERALS)], &[FORBID], &errors, &[]);
}

/// An alias whose expansion has type arguments and is not an array, and a generic class itself,
/// never accepted type arguments on a class literal. The useless `Str<Int, Int>` beside them keeps
/// its warning, which kotlinc does not print beside an error.
#[test]
fn class_literal_of_a_generic_alias_with_type_arguments_is_an_error() {
    let source = "\
typealias Str = String
typealias Lst<T> = List<T>

val l = Lst<Int>::class
val m = List<Int>::class
val n = Str<Int, Int>::class
";
    assert_matches_kotlinc(
        &[("Generic.kt", source)],
        &[],
        &[
            format!("Generic.kt:4:9: {NOT_A_CLASS}"),
            format!("Generic.kt:5:9: {NOT_A_CLASS}"),
        ],
        &[],
    );
}
