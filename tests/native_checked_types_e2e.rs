//! The Native generator reads an expression's type from the frontend, not from the node's shape.
//!
//! Each program puts a value whose CHECKED type is a concrete Kotlin type behind a node whose
//! PHYSICAL form is a reference: the result of a generic function, whose declared return is a type
//! parameter. Which member answers, what a `when` merges to and which scalar is read all follow the
//! checked type; only the carrier conversion follows the physical one.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A `when` whose arms are both erased generic results merges at its checked `Int`, so the sum
/// after it is integer arithmetic rather than a question about two references.
#[test]
fn a_when_over_generic_results_merges_at_its_checked_type() {
    let source = "fun <T> id(x: T): T = x\n\
         fun pick(flag: Boolean): Int = when (flag) {\n\
         \x20   true -> id(40)\n\
         \x20   else -> id(1)\n\
         }\n\
         fun box(): String {\n\
         \x20   val sum = pick(true) + pick(false) + 1\n\
         \x20   return if (sum == 42) \"OK\" else \"fail $sum\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WhenOverGenericResults", source);
}

/// A range reached through a generic function is still a range: the receiver's checked type
/// selects the range's own `contains`, where the erased result alone names no range at all.
#[test]
fn a_range_behind_a_generic_result_is_asked_as_a_range() {
    let source = "fun <T> id(x: T): T = x\n\
         fun box(): String {\n\
         \x20   val range = id(1..5)\n\
         \x20   if (!(3 in id(1..5))) return \"fail in\"\n\
         \x20   if (6 in range) return \"fail out\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("RangeBehindGenericResult", source);
}

/// `!` on a `Boolean` that arrives boxed from a generic function: the checked type says it is a
/// `Boolean`, and the operand is unboxed to be negated.
#[test]
fn a_boolean_behind_a_generic_result_is_negated() {
    let source = "fun <T> id(x: T): T = x\n\
         fun box(): String {\n\
         \x20   if (!id(false)) return \"OK\"\n\
         \x20   return \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("BooleanBehindGenericResult", source);
}
