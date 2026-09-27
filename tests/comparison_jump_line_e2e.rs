//! kotlinc's `BooleanComparison` marks the comparison's own line right before its jump (after the
//! three-way `lcmp`/`dcmp*` of a wide comparison), so a `when` branch condition on a line of its
//! own keeps that line through its jump instead of returning to the `when`'s line. A subject
//! `when` compares at each condition's offsets, so its jumps carry the condition's line too.
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
