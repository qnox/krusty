//! KT-90169: a callable reference to a generic `vararg` function against a function type whose
//! parameter is a type variable bound to an array. From kotlinc 2.5.0 the array is the whole
//! vararg and `n` is 2; earlier releases bind it as one element and print 1.
//!
//! The case compares krusty with the kotlinc release under test (`KRUSTY_LANGUAGE_VERSION`). The
//! earlier releases keep the resolver's previous whole-vararg rule, which this change does not
//! touch, so the comparison applies from 2.5.0, the release that introduces the new rule.

use super::common;

const SOURCE: &str = "fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)\n\
fun <E> countG(vararg xs: E): Int = xs.size\n\
fun box(): String {\n\
\x20   val arr = arrayOf(\"a\", \"b\")\n\
\x20   return \"n=\" + applyIt<Array<String>>(arr, ::countG) + \" let=\" + arr.let(::countG)\n\
}\n";

#[test]
fn an_array_against_a_type_variable_is_the_whole_vararg_from_2_5() {
    let reference = common::kotlinc_box_result(SOURCE);
    if !krusty::kotlin_version::at_least(krusty::kotlin_version::KotlinVersion::V2_5_0) {
        assert_eq!(reference, "n=1 let=1", "the pre-2.5 reference result");
        return;
    }
    assert_eq!(reference, "n=2 let=2");
    let krusty = common::expect_box_run_with_stdlib(SOURCE, "generic_vararg_array_reference");
    assert_eq!(krusty, reference);
}
