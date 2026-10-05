//! kotlinc compares two `Boolean` operands with its `BooleanComparison` (`if_icmp<cond>`), even when
//! one of them is a literal. Only a `false` on the right folds afterwards, through
//! `ConstantConditionEliminationMethodTransformer`'s comparison with a known `0` on top: `b == true`
//! stays `iconst_1; if_icmpne`, `false == b` stays `iconst_0; iload; if_icmpne`, and
//! `(x == y) == false` materializes its comparison before the fold leaves an `ifne` on it. A
//! negation (`!`, a data class `equals` guard) only flips the jump of its operand.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun withFalse(b: Boolean): Int {\n\
                      \x20   if (b == false) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun withTrue(b: Boolean): Int {\n\
                      \x20   if (b == true) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun trueFirst(b: Boolean): Int {\n\
                      \x20   if (true == b) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun falseFirst(b: Boolean): Int {\n\
                      \x20   if (false == b) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun notFalse(b: Boolean): Int {\n\
                      \x20   if (b != false) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun notTrue(b: Boolean): Int {\n\
                      \x20   while (b != true) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun nested(x: Int, y: Int): Int {\n\
                      \x20   if ((x == y) == false) return 1\n\
                      \x20   return 2\n\
                      }\n\
                      \n\
                      fun negated(x: Int, y: Int, b: Boolean): Int {\n\
                      \x20   if (!(x == y)) return 1\n\
                      \x20   if (!b) return 2\n\
                      \x20   return 3\n\
                      }\n\
                      \n\
                      data class Flags(val a: Boolean, val n: Int)\n\
                      \n\
                      fun value(b: Boolean): Boolean = b == true\n\
                      \n\
                      fun subject(b: Boolean, c: Boolean): Int {\n\
                      \x20   when (b) {\n\
                      \x20       true -> return 1\n\
                      \x20       else -> return if (c) 2 else 3\n\
                      \x20   }\n\
                      }\n";

#[test]
fn boolean_literal_equality_is_byte_identical_to_kotlinc() {
    for class in ["store/BooleanLiteralEqualityKt", "store/Flags"] {
        common::byte_diff_against_kotlinc_cp(
            "BooleanLiteralEquality",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
