//! While a call's lambda is still unshaped, an already-typed argument keeps an overload alive only
//! if SOME binding of the callee's type variables could make it fit. The binding is postponed, but
//! the type constructor around it is fixed: `Array<out R>` can never accept a `List<Long>`. Keeping
//! that candidate alive let the array `zip` overload shape `{ a, b -> b - a }`, leaving `b` typed
//! by an unbound `R`; under an outer expected type the provisional `List<Nothing>` result then
//! became the lambda's expectation (`inferred type is Long but Nothing was expected`). Measured
//! against kotlinc 2.4.10.

use super::common;

#[test]
fn a_zip_lambda_in_argument_position_shapes_against_the_iterable_overload() {
    common::expect_box_ok_with_stdlib(
        "fun take(value: Any?): Any? = value\n\
fun box(): String {\n\
    val ws = listOf(1L, 3L, 6L)\n\
    val deltas = take(ws.zip(ws.drop(1)) { a, b -> b - a })\n\
    val pairs = take(ws.zip(arrayOf(\"x\", \"y\")) { a, b -> b + a })\n\
    return if (deltas == listOf(2L, 3L) && pairs == listOf(\"x1\", \"y3\")) \"OK\" else \"FAIL $deltas $pairs\"\n\
}\n",
        "ZipLambdaArgument",
    );
}

const OVERLOADS: &str = "package zl\n\
inline fun <T, R, V> Iterable<T>.over(other: Array<out R>, transform: (a: T, b: R) -> V): List<V> =\n\
    zip(other, transform)\n\
inline fun <T, R, V> Iterable<T>.over(other: Iterable<R>, transform: (a: T, b: R) -> V): List<V> =\n\
    zip(other, transform)\n";

const OVERLOAD_MAIN: &str = "import zl.*\n\
fun take(value: Any?): Any? = value\n\
fun box(): String {\n\
    val iterable = take(listOf(1L).over(listOf(5L)) { a, b -> b - a })\n\
    val array = take(listOf(1L).over(arrayOf(5L)) { a, b -> b + a })\n\
    val lengths = take(listOf(1L).over(listOf(\"xy\")) { a, b -> b.length + a })\n\
    return if (iterable == listOf(4L) && array == listOf(6L) && lengths == listOf(3L)) \"OK\"\n\
        else \"FAIL $iterable $array $lengths\"\n\
}\n";

#[test]
fn a_dependency_overload_whose_constructor_cannot_fit_does_not_shape_the_lambda() {
    common::expect_box_ok_against("postponed_constructor_overloads", OVERLOADS, OVERLOAD_MAIN);
}

/// A trailing lambda is postponed before constructor overload selection can run, so every
/// constructor whose own parameter mapping consumes the call belongs to the family that shapes
/// it, and the lambda receives the shape they share. `Box(2) { ... }` maps onto both
/// `Box(tag: String, f)` and `Box(n: Int, f)`; both give `f` the type `(Int) -> T`, so the lambda
/// is typed before final selection picks the secondary constructor and infers `trailing()`.
const SHARED_CONSTRUCTOR_SHAPE: &str = "class Box<T>(val tag: String, val f: (Int) -> T) {\n\
    \x20   constructor(n: Int, f: (Int) -> T) : this(\"n$n\", f)\n\
    }\n\
    fun trailing() = Box(2) { \"s$it\" }\n\
    fun box(): String {\n\
    \x20   val b = trailing()\n\
    \x20   val r = b.tag + b.f(3)\n\
    \x20   return if (r == \"n2s3\") \"OK\" else r\n\
    }\n";

#[test]
fn constructors_mapping_a_trailing_lambda_shape_it_together() {
    common::expect_box_same_as_kotlinc(SHARED_CONSTRUCTOR_SHAPE, "SharedConstructorShape");
}
