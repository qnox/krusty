//! A reified inline function that passes its OWN reified parameter to another reified inline
//! (`inline fun <reified T> List<Any?>.only(): List<T> = filterIsInstance<T>()`) must keep that
//! parameter reifiable in its compiled body. kotlinc splices the callee and renames its
//! `reifiedOperationMarker(3, "R")` to `"T"`, so the dependency's own caller specializes the
//! `instanceof` in turn. krusty resolved the forwarded `T` to its erased class while splicing,
//! dropped the marker, and emitted a bare `instanceof java/lang/Object`. Every caller in another
//! module then received every element (`listOf(1, "two", 3L, 4).only<Int>().sum()` threw a
//! ClassCastException). Measured against kotlinc 2.4.10.

use super::common;

const LIB: &str = "package lib\n\
inline fun <reified T> List<Any?>.only(): List<T> = filterIsInstance<T>()\n\
inline fun <reified T : Number> List<Any?>.numbers(): List<T> = filterIsInstance<T>()\n\
inline fun <reified T> List<Any?>.twice(): List<T> = only<T>()\n\
inline fun <reified T> typeName(): String = T::class.java.simpleName\n\
inline fun <reified T> className(): String = typeName<T>()\n\
inline fun <reified T> Array<out Any?>.firstMatch(): T? = firstOrNull { it is T } as T?\n";

const MAIN: &str = "import lib.*\n\
fun box(): String {\n\
    val xs = listOf(1, \"two\", 3L, 4, null)\n\
    val sum = xs.only<Int>().sum()\n\
    val longs = xs.numbers<Long>()\n\
    val strings = xs.twice<String>()\n\
    val name = className<Long>()\n\
    val first = arrayOf<Any?>(1, 2L, \"x\").firstMatch<Long>()\n\
    return if (sum == 5 && longs == listOf(3L) && strings == listOf(\"two\") && name == \"Long\" && first == 2L) \"OK\"\n\
        else \"FAIL $sum $longs $strings $name $first\"\n\
}\n";

#[test]
fn a_forwarded_reified_parameter_stays_reifiable_in_the_dependency_body() {
    let Some(result) = common::expect_box_run_against("reified_parameter_forwarding", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(result, "OK");
}
