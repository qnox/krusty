//! KT-90169: a callable reference to a generic `vararg` function against a function type whose
//! parameter is a type variable bound to an array. From kotlinc 2.5.0 the array is the whole
//! vararg and `n` is 2; earlier releases bind it as one element and print 1.
//!
//! The case pins kotlinc's result on both sides of the 2.5.0 boundary
//! (`KRUSTY_LANGUAGE_VERSION`) and compares krusty with it from 2.5.0. Before 2.5.0 kotlinc binds
//! the array as one element whenever the expected parameter comes from a call's type variable,
//! whatever the callee's result is, while a concrete expected function type takes the whole array.
//! Krusty keeps its earlier rule for those releases: the expected function type reaches reference
//! selection already substituted, without the origin of its parameter.

use super::common;

const SOURCE: &str = r#"
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)
fun <T, R> applyR(t: T, f: (T) -> R): R = f(t)
fun <E> countG(vararg xs: E): Int = xs.size
fun <E> firstG(vararg xs: E): E = xs[0]
fun <E> listG(vararg xs: E): List<E> = xs.toList()
fun tag(x: Any?): String = if (x is Array<*>) "array" else x.toString()
fun box(): String {
    val arr = arrayOf("a", "b")
    val f: (Array<String>) -> Int = ::countG
    val g: (Array<String>) -> String = ::firstG
    val h: (Array<String>) -> List<String> = ::listG
    return "count-explicit=" + applyIt<Array<String>>(arr, ::countG) +
        " count-let=" + arr.let(::countG) +
        " first-inferred=" + tag(applyR(arr, ::firstG)) +
        " first-explicit=" + tag(applyR<Array<String>, Any?>(arr, ::firstG)) +
        " list-inferred=" + applyR(arr, ::listG).size +
        " typed-count=" + f(arr) + " typed-first=" + g(arr) + " typed-list=" + h(arr).size
}
"#;

#[test]
fn an_array_against_a_type_variable_follows_the_release() {
    let reference = common::kotlinc_box_result(SOURCE);
    let whole_vararg =
        krusty::kotlin_version::at_least(krusty::kotlin_version::KotlinVersion::V2_5_0);
    let element = "count-explicit=1 count-let=1 first-inferred=array first-explicit=array \
list-inferred=1 typed-count=2 typed-first=a typed-list=2";
    let whole = "count-explicit=2 count-let=2 first-inferred=a first-explicit=a \
list-inferred=2 typed-count=2 typed-first=a typed-list=2";
    assert_eq!(reference, if whole_vararg { whole } else { element });
    if !whole_vararg {
        return;
    }
    let krusty = common::expect_box_run_with_stdlib(SOURCE, "generic_vararg_array_reference");
    assert_eq!(krusty, reference);
}
