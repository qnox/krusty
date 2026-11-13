//! KT-90169: a callable reference to a generic `vararg` function against a function type whose
//! parameter is a type variable bound to an array. From kotlinc 2.5.0 the array is the whole
//! vararg and `n` is 2; earlier releases bind it as one element and print 1.
//!
//! The case pins kotlinc's result on both sides of the boundary (`KRUSTY_LANGUAGE_VERSION`) and
//! compares krusty with it from 2.5.0. Before 2.5.0 kotlinc binds the array as one element
//! whenever the expected parameter is a call's type variable, whatever the callee's result is
//! (`firstG` and `listG` as well as `countG`), while a concrete expected function type
//! (`val f: (Array<String>) -> Int = ::countG`) already takes the whole array. Krusty keeps its
//! earlier rule for those releases because the expected function type reaches reference
//! selection already substituted, without the origin of its parameter.

use super::common;

const SOURCE: &str = "fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)\n\
fun <E> countG(vararg xs: E): Int = xs.size\n\
fun box(): String {\n\
\x20   val arr = arrayOf(\"a\", \"b\")\n\
\x20   return \"n=\" + applyIt<Array<String>>(arr, ::countG) + \" let=\" + arr.let(::countG)\n\
}\n";

#[test]
fn an_array_against_a_type_variable_follows_the_release() {
    let reference = common::kotlinc_box_result(SOURCE);
    let whole_vararg =
        krusty::kotlin_version::at_least(krusty::kotlin_version::KotlinVersion::V2_5_0);
    assert_eq!(
        reference,
        if whole_vararg { "n=2 let=2" } else { "n=1 let=1" }
    );
    if !whole_vararg {
        return;
    }
    let krusty = common::expect_box_run_with_stdlib(SOURCE, "generic_vararg_array_reference");
    assert_eq!(krusty, reference);
}
