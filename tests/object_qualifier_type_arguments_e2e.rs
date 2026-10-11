//! kotlinc's `FirStandaloneQualifierChecker` for an object qualifier written with type arguments
//! and used as a value: "object access cannot have explicit type arguments." on the `<…>` list. The
//! `Unit` object and a typealias to an object keep a deprecation warning until
//! `ForbidUselessTypeArgumentsIn25`; every other object, and a classifier with a companion object,
//! is always the error.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidUselessTypeArgumentsIn25";
const MESSAGE: &str = "object access cannot have explicit type arguments.";
const DEPRECATION: &str = "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.";

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

const UNITS: &str = "\
val h = Unit<Any>
val c = Unit<Any> == Unit
val b = listOf(Unit<Any>, kotlin.Unit<Int>)
val p = (Unit<Any>)

fun test() = Unit<Any>

@Suppress(\"EXPLICIT_TYPE_ARGUMENTS_IN_PROPERTY_ACCESS_WARNING\")
val s = Unit<Any>
";

const POSTFIX_OPERATORS: &str = "\
val refEq = Unit<Any> === Unit
val refNe = Unit<Any> !== Unit
@Suppress(\"USELESS_ELVIS\")
val elvis = Unit<Any> ?: Unit
";

#[test]
fn postfix_operators_after_type_arguments_preserve_the_qualifier_access() {
    assert_matches_kotlinc(
        &[("PostfixOperators.kt", POSTFIX_OPERATORS)],
        &[],
        &[],
        &[
            "PostfixOperators.kt:1:17",
            "PostfixOperators.kt:2:17",
            "PostfixOperators.kt:4:17",
        ]
        .map(warning),
    );
}

#[test]
fn boolean_operators_after_type_arguments_preserve_the_qualifier_access() {
    assert_matches_kotlinc(
        &[(
            "BooleanOperators.kt",
            "val and = Unit<Any> && true\nval or = Unit<Any> || false\n",
        )],
        &[FORBID],
        &[
            "BooleanOperators.kt:1:11: condition type mismatch: inferred type is 'Unit' but 'Boolean' was expected.".to_string(),
            error("BooleanOperators.kt:1:15"),
            "BooleanOperators.kt:2:10: condition type mismatch: inferred type is 'Unit' but 'Boolean' was expected.".to_string(),
            error("BooleanOperators.kt:2:14"),
        ],
        &[],
    );
}

const UNIT_LOCATIONS: [&str; 6] = [
    "Units.kt:1:13",
    "Units.kt:2:13",
    "Units.kt:3:20",
    "Units.kt:3:38",
    "Units.kt:4:14",
    "Units.kt:6:18",
];

#[test]
fn unit_access_through_type_arguments_is_deprecated() {
    assert_matches_kotlinc(
        &[("Units.kt", UNITS)],
        &[],
        &[],
        &UNIT_LOCATIONS.map(warning),
    );
}

#[test]
fn forbidden_unit_access_through_type_arguments_is_an_error() {
    let mut errors = UNIT_LOCATIONS.map(error).to_vec();
    errors.push(error("Units.kt:9:13"));
    assert_matches_kotlinc(&[("Units.kt", UNITS)], &[FORBID], &errors, &[]);
}

const OBJECTS: &str = "\
import kotlin.reflect.KProperty

object MyUnit
typealias UnitT = Unit
typealias MyUnitT = MyUnit
operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>) = \"\"
operator fun MyUnit.getValue(thisRef: Any?, property: KProperty<*>) = \"\"

object OutObject {
    object InObject
}
class OutClass<T> {
    object InObject
}
class C1 {
    companion object
}
typealias C1T = C1
typealias OutObjectInObject = OutObject.InObject
typealias OutObjectInObjectWithT<T> = OutObject.InObject
typealias TObj<K> = MyUnit
typealias TUnit<K> = Unit

val unit1 by Unit<Any>
val unit2 by UnitT<Any>
val unit3 by MyUnitT<Any>
val a = MyUnit<Any>
val b = OutObject.InObject<Any>
val c = OutClass.InObject<Any>
val d = C1<Any>
val e = C1T<Any>
val f = OutObjectInObject<Any>
val g = OutObjectInObjectWithT<Any>
val i = TObj<Nothing>
val j = TUnit<Nothing>
val k = TObj<Int, Int>
val l = Unit<Any>
val m = dep.MyUnitT<Any>
";

const DEPENDENCY_ALIAS: &str = "\
package dep

object MyUnit
typealias MyUnitT = MyUnit
";

/// A declared object, a nested object, and a classifier with a companion object, written directly
/// or through a typealias, never accepted type arguments. kotlinc prints no warnings beside an
/// error, so the deprecated forms in the same file stay silent here.
#[test]
fn object_access_through_type_arguments_is_an_error() {
    assert_matches_kotlinc(
        &[
            ("DependencyAlias.kt", DEPENDENCY_ALIAS),
            ("Objects.kt", OBJECTS),
        ],
        &[],
        &[
            "Objects.kt:27:15",
            "Objects.kt:28:27",
            "Objects.kt:29:26",
            "Objects.kt:30:11",
            "Objects.kt:31:12",
        ]
        .map(error),
        &[],
    );
}

#[test]
fn forbidden_object_access_through_type_arguments_is_an_error() {
    assert_matches_kotlinc(
        &[
            ("DependencyAlias.kt", DEPENDENCY_ALIAS),
            ("Objects.kt", OBJECTS),
        ],
        &[FORBID],
        &[
            "Objects.kt:24:18",
            "Objects.kt:25:19",
            "Objects.kt:26:21",
            "Objects.kt:27:15",
            "Objects.kt:28:27",
            "Objects.kt:29:26",
            "Objects.kt:30:11",
            "Objects.kt:31:12",
            "Objects.kt:32:26",
            "Objects.kt:33:31",
            "Objects.kt:34:13",
            "Objects.kt:35:14",
            "Objects.kt:36:13",
            "Objects.kt:37:13",
            "Objects.kt:38:20",
        ]
        .map(error),
        &[],
    );
}

/// The final provider record in a fully qualified path is the alias declaration, even though its
/// classifier facet is normalized to the target object. kotlinc reports the warning and then hits
/// the same JVM-backend qualifier bug as the unqualified aliases below, so pin the exact front-end
/// result here; the forbidden differential case above pins the same access at `Objects.kt:38:20`.
#[test]
fn qualified_object_typealias_keeps_its_selected_declaration_provenance() {
    let source = "\
package dep

object MyUnit
typealias MyUnitT = MyUnit
val qualified = dep.MyUnitT<Any>
";
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        [format!("5:28: warning: {MESSAGE} {DEPRECATION}")]
    );
}

/// A typealias to an object, written with type arguments, keeps the deprecation warning. kotlinc
/// 2.4.20 reports it too but then fails in its JVM backend on the qualifier it typed as `Unit`, so
/// this file is compared with krusty's front end alone; the forbidden run above pins the same
/// positions against kotlinc.
#[test]
fn object_typealias_access_through_type_arguments_is_deprecated() {
    let source = "\
import kotlin.reflect.KProperty

object MyUnit
typealias UnitT = Unit
typealias MyUnitT = MyUnit
operator fun Unit.getValue(thisRef: Any?, property: KProperty<*>) = \"\"
operator fun MyUnit.getValue(thisRef: Any?, property: KProperty<*>) = \"\"

object OutObject {
    object InObject
}
typealias OutObjectInObject = OutObject.InObject
typealias TObj<K> = MyUnit
typealias TUnit<K> = Unit

val unit2 by UnitT<Any>
val unit3 by MyUnitT<Any>
val f = OutObjectInObject<Any>
val i = TObj<Nothing>
val j = TUnit<Nothing>
val k = TObj<Int, Int>
";
    let expected = ["16:19", "17:21", "18:26", "19:13", "20:14", "21:13"]
        .map(|location| format!("{location}: warning: {MESSAGE} {DEPRECATION}"));
    assert_eq!(
        common::front_end_diagnostics_located(source, &[common::stdlib_jar()], None),
        expected
    );
}
