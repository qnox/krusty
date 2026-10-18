//! kotlinc's `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` for a package
//! qualifier in expression position: a parenthesized package qualifier is a deprecation warning
//! until the feature makes it an error, type arguments on an earlier package segment are a
//! deprecation warning, and with the feature an annotated or type-argument segment cannot name a
//! package at all.

use super::common;
use krusty::kotlin_version::KotlinVersion;

const FEATURE: &str =
    "-XXLanguage:+ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier";
const PARENTHESIZED: &str = "wrapping package qualifiers in parentheses is not allowed.";
const TYPE_ARGUMENTS: &str = "type arguments are not allowed for packages.";
const DEPRECATION: &str =
    "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-396.";

fn warning(location: &str, message: &str) -> String {
    format!("{location}: {message} {DEPRECATION}")
}

fn error(location: &str, message: &str) -> String {
    format!("{location}: {message}")
}

fn unresolved(location: &str, name: &str) -> String {
    format!("{location}: unresolved reference '{name}'.")
}

/// Whether the reference release has the feature. Earlier releases have neither its deprecation
/// warnings nor the language feature itself.
fn release_has_feature() -> bool {
    krusty::kotlin_version::target() >= KotlinVersion::V2_4_20
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

const DECLARATIONS: &str = "\
package part1.part2

class Cls {
    companion object { fun make() = 1 }
}
object Obj { val x = 1 }
fun top() = 2
var prop = 3

@Target(AnnotationTarget.EXPRESSION)
@Retention(AnnotationRetention.SOURCE)
annotation class Ann

class Gen<T>
typealias Alias = Cls
typealias GAlias<T> = Gen<T>
";

const PARENTHESES: &str = "\
import part1.part2.Ann

fun use() {
    val a = (part1).part2.top()
    val b = (part1.part2).Obj.x
    val c = (@Ann part1).part2.prop
    val d = ((part1).part2).Cls.make()
    val e = ((part1).part2.Cls).make()
    val f = (part1.part2.Obj).x
    val g = (part1).part2.Cls::class
    (part1).part2.prop = 4
    val h = (@Ann part1.part2).top()
    val i = (kotlin).collections.listOf(1)
    val j = (
        part1
    ).part2.top()
    val k = (part1).part2.Obj.x.plus((part1).part2.top())
}

@Suppress(\"PARENTHESIZED_PACKAGE_QUALIFIER_WARNING\")
fun suppressed() = (part1).part2.top()
";

#[test]
fn parenthesized_package_qualifiers_are_deprecated() {
    let warnings = if release_has_feature() {
        [
            "Use.kt:4:13",
            "Use.kt:5:13",
            "Use.kt:6:13",
            "Use.kt:7:13",
            "Use.kt:8:14",
            "Use.kt:10:13",
            "Use.kt:11:5",
            "Use.kt:12:13",
            "Use.kt:13:13",
            "Use.kt:14:13",
            "Use.kt:17:13",
            "Use.kt:17:38",
        ]
        .map(|location| warning(location, PARENTHESIZED))
        .to_vec()
    } else {
        Vec::new()
    };
    assert_matches_kotlinc(
        &[("Declarations.kt", DECLARATIONS), ("Use.kt", PARENTHESES)],
        &[],
        &[],
        &warnings,
    );
}

/// An annotated segment is unresolved rather than a parenthesized package, and the error is not
/// suppressible.
#[test]
fn forbidden_parenthesized_package_qualifiers_are_errors() {
    if !release_has_feature() {
        return;
    }
    let parenthesized = |location| error(location, PARENTHESIZED);
    let errors = vec![
        parenthesized("Use.kt:4:13"),
        parenthesized("Use.kt:5:13"),
        unresolved("Use.kt:6:19", "part1"),
        parenthesized("Use.kt:7:13"),
        parenthesized("Use.kt:8:14"),
        parenthesized("Use.kt:10:13"),
        parenthesized("Use.kt:11:5"),
        unresolved("Use.kt:12:25", "part2"),
        parenthesized("Use.kt:13:13"),
        parenthesized("Use.kt:14:13"),
        parenthesized("Use.kt:17:13"),
        parenthesized("Use.kt:17:38"),
        parenthesized("Use.kt:21:20"),
    ];
    assert_matches_kotlinc(
        &[("Declarations.kt", DECLARATIONS), ("Use.kt", PARENTHESES)],
        &[FEATURE],
        &errors,
        &[],
    );
}

/// Type arguments on a package segment that a later package segment follows, in a qualifier
/// that reaches a classifier. kotlinc reports the other placements with errors of its own.
const TYPE_ARGUMENTS_ON_PACKAGES: &str = "\
fun use() {
    val a = part1<Int>.part2.Obj.x
    val b = part1<Int>.part2.Cls.make()
    val c = part1<Int>.part2.Cls::class
    val d = part1<Int>.part2.Obj.x.plus(part1<String>.part2.Obj.x)
    val e = pa.pb<Int>.pc.Deep.x
    val f = pa<Int>.pb<String>.pc.Deep.x
}
";

const DEEP: &str = "\
package pa.pb.pc

object Deep { val x = 1 }
";

#[test]
fn type_arguments_on_an_earlier_package_segment_are_deprecated() {
    let warnings = if release_has_feature() {
        [
            "Use.kt:2:18",
            "Use.kt:3:18",
            "Use.kt:4:18",
            "Use.kt:5:18",
            "Use.kt:5:46",
            "Use.kt:6:18",
            "Use.kt:7:15",
        ]
        .map(|location| warning(location, TYPE_ARGUMENTS))
        .to_vec()
    } else {
        Vec::new()
    };
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Deep.kt", DEEP),
            ("Use.kt", TYPE_ARGUMENTS_ON_PACKAGES),
        ],
        &[],
        &[],
        &warnings,
    );
}

#[test]
fn a_type_argument_segment_cannot_name_a_package() {
    if !release_has_feature() {
        return;
    }
    let errors = vec![
        unresolved("Use.kt:2:13", "part1"),
        unresolved("Use.kt:3:13", "part1"),
        unresolved("Use.kt:4:13", "part1"),
        unresolved("Use.kt:5:13", "part1"),
        unresolved("Use.kt:5:41", "part1"),
        unresolved("Use.kt:6:16", "pb"),
        unresolved("Use.kt:7:13", "pa"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Deep.kt", DEEP),
            ("Use.kt", TYPE_ARGUMENTS_ON_PACKAGES),
        ],
        &[FEATURE],
        &errors,
        &[],
    );
}

/// Placements kotlinc already rejects before the feature; with it, each fails at the segment that
/// carries the list, including on the last package segment and inside parentheses.
const TYPE_ARGUMENTS_ON_FINAL_PACKAGES: &str = "\
fun use() {
    val a = part1<Int>.part2.top()
    val b = part1.part2<Int>.Obj.x
    val c = part1<Int>.part2.prop
    val d = part1.part2<String>.top()
    val e = kotlin<Int>.collections.listOf(1)
    val f = part1<Int>.part2.Obj
    val g = (part1<Int>).part2.top()
    val h = part1.part2.Obj.x
}
";

#[test]
fn a_type_argument_segment_cannot_name_the_last_package() {
    if !release_has_feature() {
        return;
    }
    let errors = vec![
        unresolved("Use.kt:2:13", "part1"),
        unresolved("Use.kt:3:19", "part2"),
        unresolved("Use.kt:4:13", "part1"),
        unresolved("Use.kt:5:19", "part2"),
        unresolved("Use.kt:6:13", "kotlin"),
        unresolved("Use.kt:7:13", "part1"),
        unresolved("Use.kt:8:14", "part1"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", TYPE_ARGUMENTS_ON_FINAL_PACKAGES),
        ],
        &[FEATURE],
        &errors,
        &[],
    );
}

/// A typealias constructor reached through a package qualifier. kotlinc prints no warnings beside
/// errors, so the forms that are errors before the feature have their own source.
const ALIAS_PARENTHESES: &str = "\
import part1.part2.Ann

fun use() {
    val a = (part1).part2.Alias()
    val b = (part1.part2).Alias()
    val c = (@Ann part1).part2.Alias()
    val d = (part1).part2.GAlias<Int>()
    val e = (part1).part2.Alias::class
    val f = part1.part2.Alias()
}
";

#[test]
fn parenthesized_package_qualifiers_of_alias_constructors_are_deprecated() {
    let warnings = if release_has_feature() {
        [
            "Use.kt:4:13",
            "Use.kt:5:13",
            "Use.kt:6:13",
            "Use.kt:7:13",
            "Use.kt:8:13",
        ]
        .map(|location| warning(location, PARENTHESIZED))
        .to_vec()
    } else {
        Vec::new()
    };
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", ALIAS_PARENTHESES),
        ],
        &[],
        &[],
        &warnings,
    );
}

/// The alias is applied only after the qualifier resolved, so an annotated segment keeps its
/// unresolved-reference error.
#[test]
fn an_alias_constructor_needs_a_resolved_package_qualifier() {
    if !release_has_feature() {
        return;
    }
    let parenthesized = |location| error(location, PARENTHESIZED);
    let errors = vec![
        parenthesized("Use.kt:4:13"),
        parenthesized("Use.kt:5:13"),
        unresolved("Use.kt:6:19", "part1"),
        parenthesized("Use.kt:7:13"),
        parenthesized("Use.kt:8:13"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", ALIAS_PARENTHESES),
        ],
        &[FEATURE],
        &errors,
        &[],
    );
}

const ALIAS_TYPE_ARGUMENTS: &str = "\
fun use() {
    val a = part1<Int>.part2.Alias()
    val b = part1.part2<Int>.Alias()
    val c = part1<Int>.part2.GAlias<String>()
    val d = part1.part2.GAlias<String>()
}
";

#[test]
fn a_type_argument_segment_cannot_name_an_alias_constructor_package() {
    if !release_has_feature() {
        return;
    }
    let errors = vec![
        unresolved("Use.kt:2:13", "part1"),
        unresolved("Use.kt:3:19", "part2"),
        unresolved("Use.kt:4:13", "part1"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", ALIAS_TYPE_ARGUMENTS),
        ],
        &[FEATURE],
        &errors,
        &[],
    );
}
