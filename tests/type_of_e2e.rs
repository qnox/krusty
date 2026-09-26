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
