//! Which operands of `==` a backend evaluates when the nullable one is `null`.
//!
//! The answers differ by platform, and each one is kotlinc's own. On the JVM, kotlinc's
//! `JvmSafeCallChainFoldingLowering` folds an equality of a safe call with a non-null primitive
//! into the call's null guard, so the other operand, on either side, is skipped for a `null`
//! receiver; any other equality evaluates both operands. Kotlin/Native's `BuiltinOperatorLowering`
//! realizes every non-IEEE `a == b` with a nullable left and a non-null right operand as
//! `a != null && a.equals(b)`: the right operand is skipped when the left one is `null`, whatever
//! form the left operand has, and a non-null left operand evaluates both.

use super::common::{expect_box_run_with_stdlib_diverging, expect_native_box};

const SOURCE: &str = "\
package store\n\
\n\
class Holder(val x: Int)\n\
\n\
var log = \"\"\n\
\n\
fun effect(tag: String): Int {\n\
    log += tag\n\
    return 1\n\
}\n\
\n\
fun text(tag: String): String {\n\
    log += tag\n\
    return \"t\"\n\
}\n\
\n\
fun check(present: Holder?, absent: Holder?, number: Int?, word: String?): String {\n\
    if (present?.x != effect(\"a\")) return \"fail present\"\n\
    if (absent?.x == effect(\"b\")) return \"fail absent left\"\n\
    if (effect(\"c\") == absent?.x) return \"fail absent right\"\n\
    if (number == effect(\"d\")) return \"fail number\"\n\
    if (word == text(\"e\")) return \"fail word\"\n\
    return log\n\
}\n\
\n\
fun box(): String = check(Holder(1), null, null, null)\n\
";

/// JVM: the two safe-call equalities fold and skip `b` and `c`; the plain nullable operands
/// evaluate both sides. Native: only a nullable LEFT operand skips the right one, so `c` runs and
/// `b`, `d` and `e` do not.
#[test]
fn a_null_operand_skips_the_other_one_as_each_platform_lowers_equality() {
    assert_eq!(
        expect_box_run_with_stdlib_diverging(SOURCE, "NullableEqualityOrder", "ac"),
        "ade"
    );
}

#[test]
fn native_skips_the_right_operand_of_a_null_left_one() {
    expect_native_box(SOURCE, "NullableEqualityOrder", "ac");
}
