//! kotlinc's `BooleanComparison` marks the comparison's own line right before its jump (after the
//! three-way `lcmp`/`dcmp*` of a wide comparison), so a `when` branch condition on a line of its
//! own keeps that line through its jump instead of returning to the `when`'s line. A subject
//! `when` compares at each condition's offsets, so its jumps carry the condition's line too.
//!
//! The reference forms mark the same line at their own deciding instruction: `BooleanComparison`'s
//! `if_acmp*` for identity, `BooleanNullCheck`'s `ifnull`/`ifnonnull`, and the `Intrinsics.areEqual`
//! call of structural equality, so an operand on a later line never keeps its line there.
use super::common;

#[test]
fn comparison_jumps_are_byte_identical_to_kotlinc() {
    let src = "package store\n\
               \n\
               fun sink(x: Int) {}\n\
               \n\
               fun grade(a: Int, b: Long, c: Double): Int {\n\
               \x20   sink(0)\n\
               \x20   return when {\n\
               \x20       a > 3 -> 1\n\
               \x20       b < 2L -> 2\n\
               \x20       c >= 0.5 -> 3\n\
               \x20       a == 0 -> 4\n\
               \x20       else -> 5\n\
               \x20   }\n\
               }\n\
               \n\
               fun kind(a: Int) {\n\
               \x20   when (a) {\n\
               \x20       1 -> sink(1)\n\
               \x20       else -> sink(2)\n\
               \x20   }\n\
               }\n\
               \n\
               fun guard(a: Int, b: Int) {\n\
               \x20   if (a >\n\
               \x20       b) sink(1)\n\
               }\n";
    common::byte_diff_against_kotlinc("ComparisonJumpLine", src, "store/ComparisonJumpLineKt")
        .expect("reference kotlinc is provisioned")
        .expect("store/ComparisonJumpLineKt byte-identical to kotlinc");
}

#[test]
fn reference_comparisons_are_byte_identical_to_kotlinc() {
    let src = "package store\n\
               \n\
               class Box(val v: Int)\n\
               \n\
               fun sink(x: Int) {}\n\
               \n\
               fun pick(b: Box?): Box? = b\n\
               \n\
               fun same(b: Box): Box = b\n\
               \n\
               fun structural(a: Box, b: Box) {\n\
               \x20   if (a ==\n\
               \x20       same(b)) sink(1)\n\
               \x20   if (a !=\n\
               \x20       same(b)) sink(2)\n\
               }\n\
               \n\
               fun structuralValue(a: String, b: String): Boolean {\n\
               \x20   val r = a ==\n\
               \x20       b + a\n\
               \x20   return r\n\
               }\n\
               \n\
               fun identityValue(a: Box, b: Box): Boolean {\n\
               \x20   val r = a ===\n\
               \x20       same(b)\n\
               \x20   return r\n\
               }\n\
               \n\
               fun nullValue(a: Box?): Boolean {\n\
               \x20   val r = a != null && null !=\n\
               \x20       pick(a)\n\
               \x20   return r\n\
               }\n\
               \n\
               fun identity(a: Box, b: Box) {\n\
               \x20   if (a ===\n\
               \x20       same(b)) sink(3)\n\
               \x20   if (a !==\n\
               \x20       same(b)) sink(4)\n\
               }\n\
               \n\
               fun nulls(a: Box?) {\n\
               \x20   if (a != null && null ==\n\
               \x20       pick(a)) sink(5)\n\
               \x20   if (a == null || null !=\n\
               \x20       pick(a)) sink(6)\n\
               }\n\
               \n\
               fun subject(a: Box, b: Box, c: Box) {\n\
               \x20   when (a) {\n\
               \x20       same(b) -> sink(7)\n\
               \x20       same(c) -> sink(8)\n\
               \x20       else -> sink(9)\n\
               \x20   }\n\
               }\n\
               \n\
               fun wrapped(a: Box, b: Box) {\n\
               \x20   sink(0)\n\
               \x20   when {\n\
               \x20       (a == same(b)) == false -> sink(10)\n\
               \x20       else -> sink(11)\n\
               \x20   }\n\
               }\n";
    common::byte_diff_against_kotlinc(
        "ReferenceComparisonLine",
        src,
        "store/ReferenceComparisonLineKt",
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/ReferenceComparisonLineKt byte-identical to kotlinc");
}
