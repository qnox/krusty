//! `typeOf<T>()` realized as kotlinc 2.4 does: a `KType` built from `kotlin.jvm.internal.Reflection`
//! factory calls, with reified arguments substituted when an inline body is expanded and
//! non-reified type parameters described by their declaring container.
use super::common;

fn run(sources: &[(&str, &str)]) -> Option<String> {
    let jdk = common::jdk_modules();
    let jars = krusty::toolchain::classpath_jars_for("// WITH_REFLECT");
    common::compile_and_run_box_files(sources, &jars, Some(jdk.as_path()))
}

/// Classifiers, nullability, projections, function and array types, reified substitution
/// (including a nullable use and an unsigned argument), a generic property's parameter, and
/// bounds that name an outer class's parameter all print as kotlinc's realization does.
#[test]
fn type_of_describes_types_and_type_parameters() {
    const MAIN: &str = r#"
import kotlin.reflect.KTypeParameter
import kotlin.reflect.typeOf

class Box<T>
class Outer<X> {
    inner class Inner<Y : X> {
        fun <Z : Y> param(): KTypeParameter =
            typeOf<Box<Z>>().arguments.single().type!!.classifier as KTypeParameter
    }
}
val <P> P.described get() = typeOf<Box<P?>>()
inline fun <reified T> describe() = typeOf<T>()
inline fun <reified T> listOfT() = typeOf<List<T?>>()

fun check(actual: Any?, expected: String): String? =
    if (actual.toString() == expected) null else "expected $expected, got $actual"

fun box(): String {
    check(typeOf<Int>(), "kotlin.Int")?.let { return it }
    check(typeOf<Int?>(), "kotlin.Int?")?.let { return it }
    check(typeOf<MutableList<in String>>(), "kotlin.collections.MutableList<in kotlin.String>")?.let { return it }
    check(typeOf<Map<String, *>>(), "kotlin.collections.Map<kotlin.String, *>")?.let { return it }
    check(typeOf<(Int, String) -> Unit>(), "(kotlin.Int, kotlin.String) -> kotlin.Unit")?.let { return it }
    check(typeOf<IntArray>(), "kotlin.IntArray")?.let { return it }
    check(describe<Array<String>>(), "kotlin.Array<kotlin.String>")?.let { return it }
    check(listOfT<UInt>(), "kotlin.collections.List<kotlin.UInt?>")?.let { return it }
    check("".described, "Box<P?>")?.let { return it }
    val z = Outer<Any>().Inner<Any>().param<Any>()
    check(z.upperBounds, "[Y]")?.let { return it }
    check((z.upperBounds.single().classifier as KTypeParameter).upperBounds, "[X]")?.let { return it }
    return "OK"
}
"#;
    assert_eq!(run(&[("main.kt", MAIN)]).expect("typeOf"), "OK");
}

/// A non-reified type parameter of an inline function from another file names that file's
/// facade as its container; kotlin-reflect resolves the function there.
#[test]
fn type_of_in_foreign_inline_function_names_its_declaring_facade() {
    const LIB: &str = r#"
package lib
import kotlin.reflect.KType
import kotlin.reflect.typeOf
inline fun <X, Y> pairOf(): KType = typeOf<Pair<X, Y>>()
"#;
    const MAIN: &str = r#"
import kotlin.reflect.KTypeParameter
fun box(): String {
    val type = lib.pairOf<String, Int>()
    val first = type.arguments.first().type!!.classifier as KTypeParameter
    return if (type.toString() == "kotlin.Pair<X, Y>" && first.upperBounds.toString() == "[kotlin.Any?]") "OK" else "fail: $type ${first.upperBounds}"
}
"#;
    assert_eq!(
        run(&[("lib.kt", LIB), ("main.kt", MAIN)]).expect("cross-file typeOf"),
        "OK"
    );
}

/// A spliced inline member from another file describes its class's type parameter with that class
/// as the container, although the calling file declares no such class.
#[test]
fn type_of_in_foreign_inline_member_names_its_declaring_class() {
    const LIB: &str = r#"
package lib
import kotlin.reflect.KType
import kotlin.reflect.typeOf
class Holder<T : CharSequence>(val value: T) {
    inline fun described(): KType = typeOf<Array<T>>()
}
"#;
    const MAIN: &str = r#"
import kotlin.reflect.KTypeParameter
fun box(): String {
    val type = lib.Holder("x").described()
    val parameter = type.arguments.first().type!!.classifier as KTypeParameter
    return if (type.toString() == "kotlin.Array<T>" && parameter.upperBounds.toString() == "[kotlin.CharSequence]") "OK" else "fail: $type ${parameter.upperBounds}"
}
"#;
    assert_eq!(
        run(&[("lib.kt", LIB), ("main.kt", MAIN)]).expect("cross-file member typeOf"),
        "OK"
    );
}

/// `+JvmSupportRecursiveTypeOf` stores a self-recursive parameter and reloads it for its bound and
/// for the type that names it. Without the flag the same source is rejected.
#[test]
fn type_of_recursive_bound_prints_the_parameter() {
    const MAIN: &str = r#"
// LANGUAGE: +JvmSupportRecursiveTypeOf
import kotlin.reflect.KTypeParameter
import kotlin.reflect.typeOf

fun <T : Comparable<T>> listed(): String {
    val parameter = typeOf<List<T>>().arguments.single().type!!.classifier as KTypeParameter
    return typeOf<List<T>>().toString() + " " + parameter.upperBounds
}

fun <T : Comparable<U>, U : Comparable<T>> paired(): String = typeOf<Pair<T, U>>().toString()

class Box<T : Comparable<T>> {
    fun ktype(): String = typeOf<List<T>>().toString()
}

fun box(): String {
    val listed = listed<Int>()
    if (listed != "kotlin.collections.List<T> [kotlin.Comparable<T>]") return listed
    if (paired<Int, Int>() != "kotlin.Pair<T, U>") return paired<Int, Int>()
    val classType = Box<Int>().ktype()
    if (classType != "kotlin.collections.List<T>") return classType
    return "OK"
}
"#;
    assert_eq!(run(&[("main.kt", MAIN)]).expect("recursive typeOf"), "OK");
}

/// The language feature is a frontend policy. Without it, reject the selected call at its source
/// span with kotlinc's complete diagnostic rather than letting JVM emission discover the cycle.
#[test]
fn recursive_type_of_requires_the_language_feature() {
    const SOURCE: &str = "import kotlin.reflect.typeOf\n\nfun <T : Comparable<T>> rejected() =\n    typeOf<List<T>>()\n";
    let expected = vec![
        "main.kt:4:5: non-reified type parameters with recursive bounds are not supported yet: T"
            .to_owned(),
    ];
    assert_eq!(
        common::reference_error_ledger(&[("main.kt", SOURCE)], &[]),
        expected
    );
    assert_eq!(
        common::krusty_error_ledger_with_args(&[("main.kt", SOURCE)], &[]),
        expected
    );
}

/// A source-local test directive must not enable a sibling file. This guards the compiler boundary
/// against collapsing finalized per-file language policy into one module-wide backend switch.
#[test]
fn recursive_type_of_feature_does_not_leak_between_files() {
    const ENABLED: &str = "// LANGUAGE: +JvmSupportRecursiveTypeOf\nimport kotlin.reflect.typeOf\nfun <T : Comparable<T>> accepted() = typeOf<List<T>>()\n";
    const DISABLED: &str = "import kotlin.reflect.typeOf\n\nfun <U : Comparable<U>> rejected() =\n    typeOf<List<U>>()\n";
    assert_eq!(
        common::krusty_error_ledger_with_args(
            &[("enabled.kt", ENABLED), ("disabled.kt", DISABLED)],
            &[],
        ),
        vec![
            "disabled.kt:4:5: non-reified type parameters with recursive bounds are not supported yet: U"
                .to_owned(),
        ]
    );
}

/// kotlinc's `generateTypeOfArguments` reads `KTypeProjection.star` and calls the class's static
/// `invariant`/`contravariant`/`covariant`, never the companion, so the facade names no
/// `KTypeProjection$Companion` and lists no row for it.
#[test]
fn type_of_projections_call_the_static_factories() {
    let source = "import kotlin.reflect.*\n\
                  class Item\n\
                  class Holder<T>\n\
                  class Couple<A, B>\n\
                  fun star() = typeOf<Holder<*>>()\n\
                  fun plain() = typeOf<Holder<Item>>()\n\
                  fun lower() = typeOf<Holder<in Item>>()\n\
                  fun upper() = typeOf<Couple<Item, out Item>>()\n";
    let compared = common::compile_with_kotlinc("Projections", source, &[], &["ProjectionsKt"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "ProjectionsKt differs from kotlinc");
}
