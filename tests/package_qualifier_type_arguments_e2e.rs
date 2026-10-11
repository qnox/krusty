//! kotlinc's `TypeArgumentsInPackagesTypeRefChecker` for type arguments written on a package
//! segment of a qualified type or annotation reference: a deprecation warning on the segment's
//! `<…>` list until `ForbidUselessTypeArgumentsIn25` makes it an error, and always an error on the
//! last package segment.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidUselessTypeArgumentsIn25";
const MESSAGE: &str = "type arguments are not allowed for packages.";
const DEPRECATION: &str =
    "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.";

fn warning(location: &str) -> String {
    format!("{location}: {MESSAGE} {DEPRECATION}")
}

fn error(location: &str) -> String {
    format!("{location}: {MESSAGE}")
}

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

const PACKAGES: &str = "\
package part1.part2.part3

annotation class Anno
class Cls
class Gen<T>
class Outer<T> {
    inner class Inner
}

@part1<Int>.part2.part3.Anno
fun target1() {}

@part1.part2<Int>.part3.Anno
fun target2() {}

fun types(a: part1<Int>.part2.part3.Cls, b: part1.part2<String>.part3.Gen<Int>): part1<Int>.part2.part3.Cls = a

val v: part1<Int>.part2.part3.Cls? = null
val w: List<part1<*>.part2.part3.Cls> = emptyList()
val x: part1<Int>.part2.part3.Outer<String>.Inner? = null

@Suppress(\"TYPE_ARGUMENTS_NOT_ALLOWED_WARNING\")
val s: part1<Int>.part2.part3.Cls? = null
";

const PACKAGE_LOCATIONS: [&str; 8] = [
    "Packages.kt:10:7",
    "Packages.kt:13:13",
    "Packages.kt:16:19",
    "Packages.kt:16:56",
    "Packages.kt:16:87",
    "Packages.kt:18:13",
    "Packages.kt:19:18",
    "Packages.kt:20:13",
];

#[test]
fn package_type_arguments_are_deprecated() {
    assert_matches_kotlinc(
        &[("Packages.kt", PACKAGES)],
        &[],
        &[],
        &PACKAGE_LOCATIONS.map(warning),
    );
}

#[test]
fn forbidden_package_type_arguments_are_errors() {
    let mut errors = PACKAGE_LOCATIONS.map(error).to_vec();
    errors.push(error("Packages.kt:23:13"));
    assert_matches_kotlinc(&[("Packages.kt", PACKAGES)], &[FORBID], &errors, &[]);
}

const LAST_PACKAGE: &str = "\
package part1.part2.part3

annotation class Anno
class Cls

@part1<Int>.part2<Int>.part3<Int>.Anno
fun target() {}

val v: part1.part2.part3<Int>.Cls? = null
val w: part1<Int>.part2.part3<String>.Cls? = null
";

/// The last package segment never accepted type arguments. kotlinc prints no warnings beside an
/// error, so the earlier segments' deprecations stay silent here.
#[test]
fn last_package_segment_type_arguments_are_an_error() {
    assert_matches_kotlinc(
        &[("LastPackage.kt", LAST_PACKAGE)],
        &[],
        &[
            "LastPackage.kt:6:29",
            "LastPackage.kt:9:25",
            "LastPackage.kt:10:30",
        ]
        .map(error),
        &[],
    );
}

#[test]
fn forbidden_last_package_segment_type_arguments_are_errors() {
    assert_matches_kotlinc(
        &[("LastPackage.kt", LAST_PACKAGE)],
        &[FORBID],
        &[
            "LastPackage.kt:6:7",
            "LastPackage.kt:6:18",
            "LastPackage.kt:6:29",
            "LastPackage.kt:9:25",
            "LastPackage.kt:10:13",
            "LastPackage.kt:10:30",
        ]
        .map(error),
        &[],
    );
}
