//! kotlinc's checks on how a package qualifier is written: parentheses around part of one, type
//! arguments on a segment, and an annotated part are deprecation warnings until
//! `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` makes the parentheses an error
//! and stops resolution at the segment with type arguments or an annotation.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier";

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[String], warnings: &[String]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("PackageQualifiers.kt", source)],
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

const MESSAGE: &str = "wrapping package qualifiers in parentheses is not allowed.";

/// The reported positions in [`QUALIFIERS`].
const POSITIONS: [&str; 10] = [
    "16:5", "17:5", "18:5", "19:13", "20:13", "21:13", "23:13", "24:14", "25:5", "26:13",
];

const QUALIFIERS: &str = "\
package p1.p2.p3

fun foo() {}
var counter = 0
class Outer {
    object Nested {
        val x = 1
    }
}
object Obj {
    val y = 2
}

fun test() {
    p1.p2.p3.foo()
    (p1).p2.p3.foo()
    (p1.p2).p3.foo()
    (p1.p2.p3).foo()
    val a = ((p1).p2.p3).Outer.Nested
    val b = ((p1.p2).p3).Outer.Nested
    val c = (((p1).p2).p3).Outer.Nested
    val d = (p1.p2.p3.Outer).Nested
    val e = (p1.p2.p3).Obj.y
    val f = ((p1.p2.p3).Outer).Nested.x
    (p1.p2.p3).counter = 1
    val g = (p1.p2.p3).counter
}
";

#[test]
fn parenthesized_package_qualifiers_are_a_deprecation_warning() {
    let warnings = POSITIONS
        .iter()
        .map(|position| {
            format!(
                "{position}: {MESSAGE} This will become an error in language version 2.5. See \
                 https://youtrack.jetbrains.com/issue/KTLC-396."
            )
        })
        .collect::<Vec<_>>();
    assert_matches_kotlinc(QUALIFIERS, &[], &[], &warnings);
}

#[test]
fn forbidden_parenthesized_package_qualifiers_are_an_error() {
    let errors = POSITIONS
        .iter()
        .map(|position| format!("{position}: {MESSAGE}"))
        .collect::<Vec<_>>();
    assert_matches_kotlinc(QUALIFIERS, &[FORBID], &errors, &[]);
}

const ANNOTATED: &str = "\
package p1.p2.p3

@Retention(AnnotationRetention.SOURCE)
@Target(AnnotationTarget.EXPRESSION)
annotation class Marker

fun foo() {}
class Foo {
    object Bar
}

fun test() {
    (@Marker p1).p2.p3.foo()
    (@Marker p1.p2.p3).foo()
    val a = (@Marker p1.p2.p3).Foo.Bar
    val b = (@Marker p1.p2.p3.Foo).Bar
    @Marker p1.p2.p3.foo()
    p1<Int>.p2.p3.foo()
}
";

#[test]
fn annotated_and_type_argument_package_qualifiers_are_a_deprecation_warning() {
    let suffix = "This will become an error in language version 2.5. See \
                  https://youtrack.jetbrains.com/issue/KTLC-396.";
    assert_matches_kotlinc(
        ANNOTATED,
        &[],
        &[],
        &[
            format!("13:5: {MESSAGE} {suffix}"),
            format!("14:5: {MESSAGE} {suffix}"),
            format!("15:13: {MESSAGE} {suffix}"),
            format!("18:7: type arguments are not allowed for packages. {suffix}"),
        ],
    );
}

#[test]
fn forbidden_annotated_and_type_argument_package_segments_are_unresolved() {
    assert_matches_kotlinc(
        ANNOTATED,
        &[FORBID],
        &[
            "13:14: unresolved reference 'p1'.".to_string(),
            "14:20: unresolved reference 'p3'.".to_string(),
            "15:28: unresolved reference 'p3'.".to_string(),
            "18:5: unresolved reference 'p1'.".to_string(),
        ],
        &[],
    );
}

/// Parentheses end a package qualifier where resolution then stops at a segment with type
/// arguments: both are reported.
#[test]
fn forbidden_parentheses_before_an_unresolved_segment_are_reported() {
    let source = "\
package p1.p2.p3

fun foo() {}

fun test() {
    (p1).p2<Int>.p3.foo()
}
";
    assert_matches_kotlinc(
        source,
        &[FORBID],
        &[
            format!("6:5: {MESSAGE}"),
            "6:10: unresolved reference 'p2'.".to_string(),
        ],
        &[],
    );
}

const CALLABLE_REFERENCES: &str = "\
package p1.p2.p3

class Foo {
    fun m() {}
}

fun test() {
    val a = (p1.p2.p3).Foo::class
    val b = (p1.p2).p3.Foo::m
    val c = p1<Int>.p2.p3.Foo::class
}
";

/// The receiver of a callable reference or class literal is a package qualifier like any other.
#[test]
fn callable_reference_package_qualifiers_are_a_deprecation_warning() {
    let suffix = "This will become an error in language version 2.5. See \
                  https://youtrack.jetbrains.com/issue/KTLC-396.";
    assert_matches_kotlinc(
        CALLABLE_REFERENCES,
        &[],
        &[],
        &[
            format!("8:13: {MESSAGE} {suffix}"),
            format!("9:13: {MESSAGE} {suffix}"),
            format!("10:15: type arguments are not allowed for packages. {suffix}"),
        ],
    );
}

#[test]
fn forbidden_callable_reference_package_qualifiers_are_reported() {
    assert_matches_kotlinc(
        CALLABLE_REFERENCES,
        &[FORBID],
        &[
            format!("8:13: {MESSAGE}"),
            format!("9:13: {MESSAGE}"),
            "10:13: unresolved reference 'p1'.".to_string(),
        ],
        &[],
    );
}
