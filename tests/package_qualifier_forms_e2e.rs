//! kotlinc's `ForbidAnnotationsTypeArgumentsAndParenthesesForPackageQualifier` for a package
//! qualifier in expression position: a parenthesized package qualifier is a deprecation warning
//! until the feature makes it an error, type arguments on an earlier package segment are a
//! deprecation warning, and with the feature an annotated or type-argument segment cannot name a
//! package at all. `-progressive` turns the feature on, through the CLI on the JVM and through the
//! release's progressive feature set on the KLIB targets.

use super::common;
use krusty::compilation_target::CompilationTarget;
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
    for selection in [FEATURE, "-progressive"] {
        assert_matches_kotlinc(
            &[("Declarations.kt", DECLARATIONS), ("Use.kt", PARENTHESES)],
            &[selection],
            &errors,
            &[],
        );
    }
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
    for selection in [FEATURE, "-progressive"] {
        assert_matches_kotlinc(
            &[
                ("Declarations.kt", DECLARATIONS),
                ("Deep.kt", DEEP),
                ("Use.kt", TYPE_ARGUMENTS_ON_PACKAGES),
            ],
            &[selection],
            &errors,
            &[],
        );
    }
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

const VALUE_RECEIVERS: &str = "\
package part1.part2

class Holder<T> {
    object O { val z = 1; fun f() = 2 }
    class Nested { companion object { val w = 1; fun cf() = 3 } }
    enum class En { A; companion object { val q = 1 } }
}
";

/// A classifier qualifier whose FIR type arguments are inherited from an outer classifier does not
/// supply its object or companion value as a receiver (`FirTowerResolveTask`
/// `runResolverForQualifierReceiver`), so a member of that value is unresolved; the static scope
/// (an enum entry, `values()`) still answers.
const VALUE_RECEIVER_USES: &str = "\
fun use() {
    val a = part1.part2.Holder<Int>.O.z
    val b = part1.part2.Holder<Int>.O.f()
    val c = part1.part2.Holder<Int>.Nested.w
    val d = part1.part2.Holder<Int>.Nested.cf()
    val e = part1.part2.Holder<Int>.En.A
    val f = part1.part2.Holder<Int>.En.q
    val g = part1.part2.Holder<Int>.En.values()
    val h = part1.part2.Holder<Int>.Nested.Companion.w
    part1.part2.Holder<Int>.O.z.toString()
    val i = part1.part2.Holder.O.z
}
";

#[test]
fn a_qualifier_with_type_arguments_has_no_value_receiver() {
    let outer = |location| error(location, OUTER_CLASS_TYPE_ARGUMENTS);
    let errors = vec![
        outer("Use.kt:2:13"),
        unresolved("Use.kt:2:39", "z"),
        outer("Use.kt:3:13"),
        unresolved("Use.kt:3:39", "f"),
        outer("Use.kt:4:13"),
        unresolved("Use.kt:4:44", "w"),
        outer("Use.kt:5:13"),
        unresolved("Use.kt:5:44", "cf"),
        outer("Use.kt:6:13"),
        outer("Use.kt:7:13"),
        unresolved("Use.kt:7:40", "q"),
        outer("Use.kt:8:13"),
        outer("Use.kt:9:13"),
        outer("Use.kt:9:13"),
        unresolved("Use.kt:9:54", "w"),
        outer("Use.kt:10:5"),
        unresolved("Use.kt:10:31", "z"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", VALUE_RECEIVERS),
            ("Use.kt", VALUE_RECEIVER_USES),
        ],
        &[],
        &errors,
        &[],
    );
}

const EXPRESSION_EXPECTED: &str = "expression expected, but package name found.";

/// A package qualifier used as an expression (`FirStandaloneQualifierChecker`), including as the
/// left-hand side of a callable reference, which is no qualified access of kotlinc's checker
/// context. It is reported at the package's last segment. The reference itself is still resolved
/// in the package's callable scope, so a name with no top-level callable or constructor there is
/// unresolved.
const PACKAGE_EXPRESSIONS: &str = "\
fun use() {
    val a = part1.part2
    val b = kotlin
    val c = (part1.part2)
    val d = part1<Int>.part2
    println(part1.part2)
    val e = part1.part2::top
    val f = part1.part2<Int>::top
    val g = (part1.part2<Int>)::top
    val h = part1<Int>.part2<Int>::top
    val i = part1.part2::prop
    val j = part1<Int>::part2
    val k = part1.part2::Obj
    val l = part1.part2::Cls
}
";

#[test]
fn a_package_qualifier_is_not_an_expression() {
    let expected = |location| error(location, EXPRESSION_EXPECTED);
    let errors = vec![
        expected("Use.kt:2:19"),
        expected("Use.kt:3:13"),
        expected("Use.kt:4:20"),
        error("Use.kt:5:13", OUTER_CLASS_TYPE_ARGUMENTS),
        expected("Use.kt:5:24"),
        expected("Use.kt:6:19"),
        expected("Use.kt:7:19"),
        error("Use.kt:8:13", TYPE_ARGUMENTS),
        expected("Use.kt:8:19"),
        error("Use.kt:9:14", TYPE_ARGUMENTS),
        expected("Use.kt:9:20"),
        error("Use.kt:10:13", OUTER_CLASS_TYPE_ARGUMENTS),
        error("Use.kt:10:13", TYPE_ARGUMENTS),
        expected("Use.kt:10:24"),
        expected("Use.kt:11:19"),
        expected("Use.kt:12:13"),
        error("Use.kt:12:13", TYPE_ARGUMENTS),
        unresolved("Use.kt:12:25", "part2"),
        expected("Use.kt:13:19"),
        unresolved("Use.kt:13:26", "Obj"),
        expected("Use.kt:14:19"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", DECLARATIONS),
            ("Use.kt", PACKAGE_EXPRESSIONS),
        ],
        &[],
        &errors,
        &[],
    );
}

const GENERIC_QUALIFIER_ON_CONSTRUCTOR_CALL: &str = "usage of a qualifier with type arguments to \
     call nested class constructor is deprecated. The type arguments must be removed.";

const CONSTRUCTED: &str = "\
package part1.part2

class Made
class Shell<T> {
    class Nested { class Deep }
    class Gen<U>
}
typealias MadeAlias = Made
";

/// `FirGenericQualifierOnConstructorCallChecker`: the constructor of a class that is not `inner`
/// called through a qualifier with FIR type arguments, at the qualifier's own list or, for
/// inherited arguments, at the qualifier.
const QUALIFIED_CONSTRUCTOR_CALLS: &str = "\
fun use() {
    val a = part1.part2.Shell<Int>.Nested.Deep()
    val b = part1<Int>.part2<String>.Made()
    val c = part1.part2.Shell<Int>.Gen<String>()
    val d = part1.part2.Shell<Int>.Nested<String>.Deep()
    val e = (part1.part2<Int>).Made()
    val f = part1.part2<Int>.MadeAlias()
    val g = part1<Int>.part2.Made()
    val h = part1.part2.Shell<Int>.Nested()
}
";

#[test]
fn a_qualifier_with_type_arguments_cannot_call_a_nested_constructor() {
    let generic = |location| error(location, GENERIC_QUALIFIER_ON_CONSTRUCTOR_CALL);
    let outer = |location| error(location, OUTER_CLASS_TYPE_ARGUMENTS);
    let packages = |location| error(location, TYPE_ARGUMENTS);
    let errors = vec![
        generic("Use.kt:2:13"),
        outer("Use.kt:2:13"),
        outer("Use.kt:3:13"),
        packages("Use.kt:3:13"),
        generic("Use.kt:3:29"),
        generic("Use.kt:4:30"),
        outer("Use.kt:5:13"),
        generic("Use.kt:5:42"),
        packages("Use.kt:6:14"),
        generic("Use.kt:6:25"),
        packages("Use.kt:7:13"),
        generic("Use.kt:7:24"),
        outer("Use.kt:8:13"),
        generic("Use.kt:9:30"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", CONSTRUCTED),
            ("Use.kt", QUALIFIED_CONSTRUCTOR_CALLS),
        ],
        &[],
        &errors,
        &[],
    );
}

/// The constructor call through a typealias declared in a local class continues the local class
/// the receiver resolved to; its hoisted runtime name is never spelled.
const LOCAL_CLASS_ALIAS: &str = "\
package local.aliases

class Item(val value: String)

fun use(): String {
    class Local {
        typealias ItemAlias = Item
    }
    return Local.ItemAlias(\"OK\").value
}
";

#[test]
fn a_local_class_alias_constructor_continues_its_owner() {
    assert_matches_kotlinc(
        &[("LocalAlias.kt", LOCAL_CLASS_ALIAS)],
        &["-XXLanguage:+LocalTypeAliases"],
        &[],
        &[],
    );
}

const PACKAGE_SCOPE: &str = "\
package part1.part2

fun top() = 2
val prop = 3
";

/// A package receiver resolves its callee in the package's scope alone, so a member of `Any` or a
/// property whose value cannot be invoked is unresolved at the callee. A package qualifier used as
/// an expression is typed `Unit`: an expected type mismatches it and a safe call selects on it.
const PACKAGE_RECEIVER_USES: &str = "\
fun use() {
    val a = part1.part2.toString()
    val b = part1.part2.prop()
    val c = (part1.part2).top()
    val d: Int = part1.part2
    val e = kotlin?.toString()
    val f = part1.part2?.top()
}
";

#[test]
fn a_package_receiver_resolves_its_callee_in_the_package_scope() {
    let expected = |location| error(location, EXPRESSION_EXPECTED);
    let on_unit = if release_has_feature() {
        "unresolved reference 'top' on receiver of type 'Unit'."
    } else {
        "unresolved reference 'top'."
    };
    let errors = vec![
        unresolved("Use.kt:2:25", "toString"),
        unresolved("Use.kt:3:25", "prop"),
        error(
            "Use.kt:5:16",
            "initializer type mismatch: expected 'Int', actual 'Unit'.",
        ),
        expected("Use.kt:5:24"),
        expected("Use.kt:6:13"),
        expected("Use.kt:7:19"),
        error("Use.kt:7:26", on_unit),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", PACKAGE_SCOPE),
            ("Use.kt", PACKAGE_RECEIVER_USES),
        ],
        &[],
        &errors,
        &[],
    );
}

const INNER_CLASSES: &str = "\
package part1.part2

interface Marker<A>
class Shell<T : CharSequence, in Q> {
    class Mid<V> {
        inner class Deep<out W>
    }
    inner class Inner<U> : Marker<T>, Comparable<U> {
        override fun compareTo(other: U) = 0
    }
    fun own() = Shell.Inner<Int>()
}
class Plain {
    inner class I
    class Nested { inner class In }
}
";

/// A classifier qualifier has no value, so the constructor of an `inner` class it selects has no
/// dispatch receiver even inside the outer class; the call has no type to check further.
const INNER_CONSTRUCTOR_CALLS: &str = "\
fun use() {
    val a = part1.part2.Plain.I()
    val b = part1.part2.Shell.Mid.Deep<Int>()
    val c = part1.part2.Shell<String, Int>.Inner<Int>()
    val d: String = part1.part2.Shell.Inner<Int>()
    val e = part1.part2.Shell.Inner<Int>().hashCode()
    val f = part1.part2.Shell<String, Int>.Mid.Deep<Int>()
    val g = part1.part2.Plain.Nested.In()
}
";

#[test]
fn an_inner_constructor_through_a_qualifier_has_no_receiver() {
    let no_receiver = |location, class: &str| {
        error(
            location,
            &format!(
                "constructor of the inner class '{class}' can only be called with a receiver of \
                 the containing class."
            ),
        )
    };
    let inner =
        "inner class Inner<U, Outer(T) : CharSequence, in Outer(Q)> : Marker<T>, Comparable<U>";
    let deep = "inner class Deep<out W, Outer(V)> : Any";
    let errors = vec![
        no_receiver("Declarations.kt:11:23", inner),
        no_receiver("Use.kt:2:31", "inner class I : Any"),
        no_receiver("Use.kt:3:35", deep),
        no_receiver("Use.kt:4:44", inner),
        no_receiver("Use.kt:5:39", inner),
        no_receiver("Use.kt:6:31", inner),
        error("Use.kt:7:13", OUTER_CLASS_TYPE_ARGUMENTS),
        no_receiver("Use.kt:7:48", deep),
        no_receiver("Use.kt:8:38", "inner class In : Any"),
    ];
    assert_matches_kotlinc(
        &[
            ("Declarations.kt", INNER_CLASSES),
            ("Use.kt", INNER_CONSTRUCTOR_CALLS),
        ],
        &[],
        &errors,
        &[],
    );
}

/// One file holding a parenthesized, an annotated, and a type-argument package qualifier, whose
/// result changes with the feature: accepted with deprecation warnings without it, three errors
/// with it.
const PROGRESSIVE_QUALIFIERS: &str = "\
package part1.part2

@Target(AnnotationTarget.EXPRESSION)
@Retention(AnnotationRetention.SOURCE)
annotation class Ann

object Obj { val x = 1 }
fun top() = 2

fun use() {
    val a = (part1).part2.top()
    val b = (@Ann part1).part2.top()
    val c = part1<Int>.part2.Obj.x
}
";

const KLIB_TARGETS: [CompilationTarget; 3] = [
    CompilationTarget::WasmJs,
    CompilationTarget::WasmWasi,
    CompilationTarget::Native,
];

/// kotlinc's `-progressive`: the reference release's language defaults with every progressive
/// feature of its table on.
fn progressive_features() -> krusty::features::LangFeatures {
    let table = krusty::features::FeatureTable::for_version(krusty::kotlin_version::target())
        .expect("the reference release has a feature table");
    let mut features = krusty::features::LangFeatures::new();
    for feature in table.progressive() {
        features.set(&feature.name, true);
    }
    features
}

/// Krusty's outcome for `PROGRESSIVE_QUALIFIERS` analyzed for `target` under `features`, each
/// error in the reference compilers' `Q.kt:line:column: message` shape, in emission order.
fn klib_outcome(
    target: CompilationTarget,
    features: &krusty::features::LangFeatures,
) -> common::CompilerOutcome {
    let errors = common::front_end_diagnostic_records_with_features_for(
        target,
        PROGRESSIVE_QUALIFIERS,
        &[common::stdlib_jar()],
        None,
        features,
    )
    .iter()
    .filter(|diagnostic| diagnostic.severity == krusty::diag::Severity::Error)
    .map(|diagnostic| {
        let (line, column) = krusty::diag::line_col(PROGRESSIVE_QUALIFIERS, diagnostic.span.lo);
        format!("Q.kt:{line}:{column}: {}", diagnostic.msg)
    })
    .collect::<Vec<_>>();
    common::CompilerOutcome {
        accepted: errors.is_empty(),
        errors,
    }
}

/// With `-progressive` each KLIB target's reference compiler rejects the three qualifiers with
/// exactly the errors krusty reports under the release's progressive features; without it both
/// accept the file. The errors are pinned too, so a target whose reference compiler is not
/// provisioned still checks krusty's.
#[test]
fn progressive_package_qualifiers_match_every_klib_reference_compiler() {
    if !release_has_feature() {
        return;
    }
    let rejected = common::CompilerOutcome {
        accepted: false,
        errors: vec![
            error("Q.kt:11:13", PARENTHESIZED),
            unresolved("Q.kt:12:19", "part1"),
            unresolved("Q.kt:13:13", "part1"),
        ],
    };
    let accepted = common::CompilerOutcome {
        accepted: true,
        errors: Vec::new(),
    };
    let sources = [("Q.kt", PROGRESSIVE_QUALIFIERS)];
    for target in KLIB_TARGETS {
        for (args, features, expected) in [
            (
                vec!["-progressive".to_string()],
                progressive_features(),
                &rejected,
            ),
            (Vec::new(), krusty::features::LangFeatures::new(), &accepted),
        ] {
            assert_eq!(
                &klib_outcome(target, &features),
                expected,
                "{target:?} {args:?}"
            );
            if let Some(reference) = common::reference_outcome(target, &sources, &args) {
                assert_eq!(&reference, expected, "{target:?} reference {args:?}");
            }
        }
    }
}
