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

const OUTER_CLASS_TYPE_ARGUMENTS: &str =
    "type arguments for outer class are redundant when nested class is referenced.";

fn nested_via_instance(location: &str, classifier: &str) -> String {
    format!("{location}: nested {classifier} accessed via instance reference.")
}

/// Before the feature, the qualifier a segment continues carries its predecessor's type
/// arguments as an outer-class error, and a package receiver of an access that has type arguments
/// of its own is rejected. kotlinc reports the standalone `part1<Int>.part2.Obj` twice: once on the
/// `part2` qualifier and once on the `Obj` value that keeps it as its explicit parent.
#[test]
fn type_arguments_on_the_last_packages_are_errors() {
    let errors = vec![
        error("Use.kt:2:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:3:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:4:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:5:13", TYPE_ARGUMENTS),
        error("Use.kt:6:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:7:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:7:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:8:13", OUTER_CLASS_TYPE_ARGUMENTS),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", TYPE_ARGUMENTS_ON_FINAL_PACKAGES),
        ],
        &[],
        &errors,
        &[],
    );
}

const OUTER: &str = "\
package part1.part2

class Outer<T> {
    class Nested {
        object Deep { val y = 1 }
        companion object
    }
    inner class Inner { fun g() = 1 }
    object O { val z = 1 }
    enum class En { A; companion object }
    interface Face { companion object }
    companion object
}
";

/// The FIR qualifiers of package and classifier chains: a receiver segment continues its
/// predecessor (an outer-class error when the predecessor has type arguments, which a nested
/// classifier also inherits), a standalone classifier value keeps its predecessor and that
/// predecessor's errors, and is a nested classifier accessed via an instance when the predecessor
/// has type arguments. An `inner` class is exempt from the outer-class error.
const TYPE_ARGUMENTS_ON_QUALIFIERS: &str = "\
fun use() {
    val a = part1<Int>.part2<String>.top()
    val b = part1.part2<Int>.Obj
    val c = part1<Int>.part2<String>.Obj.x
    val d = part1.part2<Int>.Cls::class
    val e = part1.part2<Int>.Cls.make()
    val f = part1.part2<Int>.prop
    part1<Int>.part2.prop = 4
    part1.part2<Int>.prop = 5
    val g = part1<Int>.part2.Alias()
    val h = (part1.part2<Int>).top()
    val i = part1<Int>.part2.Cls
    val j = part1.part2<Int>.Cls
    val k = part1<Int>.part2<String>.Obj
    val l = part1.part2.Outer<Int>.O
    val m = part1.part2<Int>.Obj::x
    val n = part1.part2<Int>.Alias
    val o = part1.part2<Int>.Cls.Companion
    val p = part1.part2.Outer<Int>.Nested.Deep
    val q = part1.part2.Outer<Int>.Nested
    val r = part1.part2<Int>.Cls.Companion.make()
    val s = part1.part2.Outer<Int>.En
    val t = part1.part2.Outer<Int>.Face
    val u = part1.part2<Int>.Outer.Companion
    val v = part1.part2.Outer<Int>.Companion
    val w = part1.part2.Outer<Int>.Inner::g
    val y = (part1<Int>).part2.Obj
}
";

#[test]
fn type_arguments_on_qualifier_segments_are_errors() {
    let outer = |location| error(location, OUTER_CLASS_TYPE_ARGUMENTS);
    let packages = |location| error(location, TYPE_ARGUMENTS);
    let errors = vec![
        outer("Use.kt:2:13"),
        packages("Use.kt:2:13"),
        nested_via_instance("Use.kt:3:30", "class 'Obj'"),
        outer("Use.kt:4:13"),
        outer("Use.kt:5:13"),
        outer("Use.kt:6:13"),
        packages("Use.kt:7:13"),
        outer("Use.kt:8:5"),
        packages("Use.kt:9:5"),
        outer("Use.kt:10:13"),
        packages("Use.kt:11:14"),
        outer("Use.kt:12:13"),
        outer("Use.kt:12:13"),
        nested_via_instance("Use.kt:13:30", "class 'Cls'"),
        outer("Use.kt:14:13"),
        outer("Use.kt:14:13"),
        nested_via_instance("Use.kt:14:38", "class 'Obj'"),
        nested_via_instance("Use.kt:15:36", "class 'O'"),
        outer("Use.kt:16:13"),
        nested_via_instance("Use.kt:17:30", "typealias 'Alias'"),
        outer("Use.kt:18:13"),
        outer("Use.kt:18:13"),
        outer("Use.kt:19:13"),
        outer("Use.kt:19:13"),
        nested_via_instance("Use.kt:19:43", "class 'Deep'"),
        nested_via_instance("Use.kt:20:36", "class 'Nested'"),
        outer("Use.kt:21:13"),
        nested_via_instance("Use.kt:22:36", "enum class 'En'"),
        nested_via_instance("Use.kt:23:36", "interface 'Face'"),
        outer("Use.kt:24:13"),
        outer("Use.kt:24:13"),
        nested_via_instance("Use.kt:25:36", "companion object 'Companion'"),
        outer("Use.kt:27:13"),
        outer("Use.kt:27:13"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Outer.kt", OUTER),
            ("Use.kt", TYPE_ARGUMENTS_ON_QUALIFIERS),
        ],
        &[],
        &errors,
        &[],
    );
}

/// A classifier value keeps its package qualifier, which is then checked for parentheses and
/// type arguments like a receiver; a classifier continued from a package checks its receiver.
const SELECTED_VALUE_FORMS: &str = "\
fun use() {
    val a = (part1).part2.Obj
    val b = (part1<Int>).part2.Obj.x
    val c = part1<Int>.part2.Obj.x
    val d = (part1).part2.Cls.Companion
    val e = part1<Int>.part2.Cls.Companion.make()
}
";

#[test]
fn package_qualifiers_of_selected_values_are_deprecated() {
    let warnings = if release_has_feature() {
        vec![
            warning("Use.kt:2:13", PARENTHESIZED),
            warning("Use.kt:3:13", PARENTHESIZED),
            warning("Use.kt:4:18", TYPE_ARGUMENTS),
            warning("Use.kt:5:13", PARENTHESIZED),
            warning("Use.kt:6:18", TYPE_ARGUMENTS),
        ]
    } else {
        Vec::new()
    };
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", SELECTED_VALUE_FORMS),
        ],
        &[],
        &[],
        &warnings,
    );
}

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
