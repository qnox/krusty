//! A type variable with only incompatible upper bounds is the intersection of those
//! bounds. The call still returns a value; it is not a divergent `Nothing`.

use super::common;

const INTERSECT: &str = "\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun witness(n: Int, s: String) { }\n\
fun check() {\n\
    val a = intersect(In<Int>(), In<String>())\n\
    witness(a, a)\n\
}\n\
fun box(): String {\n\
    val a = intersect(In<Int>(), In<String>())\n\
    val b = intersect(In<Int>(), *arrayOf(In<String>()))\n\
    val c = intersect(x = arrayOf(In<Int>(), In<String>()))\n\
    return if (a == null && a == b && b == c) \"OK\" else \"NOT_OK\"\n\
}\n";

#[test]
fn incompatible_upper_bounds_return_a_null_intersection() {
    common::expect_box_same_as_kotlinc(INTERSECT, "EmptyIntersection");
}

#[test]
fn an_intersection_is_not_nothing_without_an_expected_nothing() {
    const SRC: &str = "\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun take(x: Nothing): Any = x\n\
fun box(): String {\n\
    val a = intersect(In<Int>(), In<String>())\n\
    take(a)\n\
    return \"OK\"\n\
}\n";
    common::assert_errors_match_kotlinc(&[("EmptyIntersectionNothing.kt", SRC)], &[]);
}

#[test]
fn one_contravariant_upper_bound_is_that_bound() {
    const SRC: &str = "\
class Context<T>\n\
fun <T> select(value: Context<in T>): T = null as T\n\
fun box(): String {\n\
    val a = select(Context<Any>())\n\
    return if (a == null) \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "SingleUpperBound");
}

#[test]
fn an_expected_nothing_result_still_instantiates_nothing() {
    const SRC: &str = "\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun probe(): Nothing = intersect(In<Int>(), In<String>())\n\
fun box(): String = \"OK\"\n";
    common::expect_box_same_as_kotlinc(SRC, "ExpectedNothingIntersection");
}
