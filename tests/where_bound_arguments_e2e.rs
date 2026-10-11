//! A type variable is a subtype of every one of its upper bounds, including those a `where` clause
//! adds after the first: it is an argument for a parameter of any of them, through another type
//! variable's bounds too, and selects an operator such as `contains` the same way.

use super::common;

#[test]
fn a_type_variable_is_an_argument_for_any_of_its_bounds() {
    let src = r#"fun takeCs(c: CharSequence) = c.length
fun takeU(u: UInt) = u.toInt()
fun <T> second(a: T) where T : Comparable<T>, T : CharSequence = takeCs(a)
fun <T : X, X> chained(a: T) where X : Comparable<X>, X : CharSequence = takeCs(a)
fun <T : X, X : Comparable<UInt>> unsigned(a: T) where X : UInt = takeU(a)
fun <T : X, X : Comparable<UInt>> contained(a: T): Boolean where X : UInt = (1U..10U).contains(a)
fun <T : X, X : Comparable<Int>> member(a: T): Boolean where X : Int = a in 1..10

fun box(): String {
    val results = listOf(second("ab"), chained("abc"), unsigned(4U))
    if (results != listOf(2, 3, 4)) return "$results"
    if (!contained(3U) || contained(11U)) return "contains"
    if (!member(3) || member(0)) return "in"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_arguments");
}
