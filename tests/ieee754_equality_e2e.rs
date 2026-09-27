//! kotlinc's `Ieee754Equals` intrinsic always materializes the Boolean of `==` between two
//! non-null `Float`s or `Double`s (`dcmpg; ifne F; iconst_1; goto E; F: iconst_0; E:`), marking the
//! comparison's line before the `dcmpg`; a condition branches on that value, and `!=` is `Not` over
//! it, which materializes again in value position. Structural `!=` on references is `Not` over
//! `Intrinsics.areEqual` the same way: a branch, not an `ixor`. Identity (`===`) and ordering keep
//! their direct compare-and-jump.
//!
//! With a nullable operand the same intrinsic calls the `Intrinsics.areEqual` overload typed by each
//! operand's nullability, `(Ljava/lang/Double;D)Z` and its siblings, instead of null tests.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    class Box(val v: Int)\n\
    \n\
    fun sink(x: Int) {}\n\
    fun half(x: Double): Double = x\n\
    fun same(b: Box): Box = b\n\
    \n\
    fun eq(a: Double, b: Double) {\n\
    \x20   if (a == b) sink(1)\n\
    }\n\
    fun ne(a: Double, b: Double) {\n\
    \x20   if (a !=\n\
    \x20       half(b)) sink(2)\n\
    }\n\
    fun neFloat(a: Float, b: Float): Boolean = a != b\n\
    fun eqFloat(a: Float, b: Float): Boolean = a == b\n\
    fun eqValue(a: Double, b: Double): Boolean {\n\
    \x20   val r = a ==\n\
    \x20       half(b)\n\
    \x20   return r\n\
    }\n\
    fun smartCast(a: Any, b: Double) {\n\
    \x20   if (a is Double && a != b) sink(3)\n\
    }\n\
    fun loop(a: Double) {\n\
    \x20   while (half(a) != a) sink(0)\n\
    }\n\
    fun zero(a: Float) {\n\
    \x20   if (a == 0.0f) sink(4)\n\
    }\n\
    fun negated(a: Double, b: Double) {\n\
    \x20   if (!(a == b)) sink(5)\n\
    }\n\
    fun subject(a: Double) {\n\
    \x20   when (a) {\n\
    \x20       1.0 -> sink(6)\n\
    \x20       half(a) -> sink(7)\n\
    \x20       else -> sink(8)\n\
    \x20   }\n\
    }\n\
    fun structuralNe(a: Box, b: Box): Boolean = a != same(b)\n\
    fun bothWays(a: Double, b: Double) = a == b || a != b + 1.0\n";

#[test]
fn ieee754_equality_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc("Ieee754Equality", SOURCE, "store/Ieee754EqualityKt")
        .expect("reference kotlinc is provisioned")
        .expect("store/Ieee754EqualityKt byte-identical to kotlinc");
}

const NULLABLE_SOURCE: &str = "package store\n\
    \n\
    fun sink(x: Int) {}\n\
    fun half(x: Double?): Double? = x\n\
    fun whole(x: Double): Double = x\n\
    \n\
    fun left(a: Double?, b: Double) {\n\
    \x20   if (a == b) sink(1)\n\
    }\n\
    fun right(a: Double, b: Double?) {\n\
    \x20   if (a != b) sink(2)\n\
    }\n\
    fun both(a: Double?, b: Double?) {\n\
    \x20   if (a ==\n\
    \x20       half(b)) sink(3)\n\
    }\n\
    fun floats(a: Float?, b: Float): Boolean = a == b\n\
    fun floatsNe(a: Float, b: Float?): Boolean = a != b\n\
    fun constant(a: Double?): Boolean = a == 1.0\n\
    fun value(a: Double?, b: Double): Boolean {\n\
    \x20   val r = a !=\n\
    \x20       whole(b)\n\
    \x20   return r\n\
    }\n\
    fun nested(a: Double?, b: Double?) {\n\
    \x20   if (a == b && a != null) sink(4)\n\
    }\n";

#[test]
fn nullable_ieee754_equality_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc(
        "NullableIeee754Equality",
        NULLABLE_SOURCE,
        "store/NullableIeee754EqualityKt",
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/NullableIeee754EqualityKt byte-identical to kotlinc");
}
