//! `is` on a SCALAR operand, in both the positions the emitter has for it.
//!
//! Kotlin has no subtyping among the primitive types, so `n is Long` where `n` is an `Int` does not
//! even compile and an `is` on a scalar reads as settled. It is not: `5 is Number` and
//! `1u is Comparable<UInt>` are true, and answering those needs the hierarchy. Each primitive's box
//! carries the descriptor that has it — an unsigned one its own, which is what makes
//! `(1u as Any) is Int` false — so boxing the operand and asking is both correct and the only rule
//! needed.
//!
//! The emitter has two shapes for the same check. In VALUE position it emits the operand, boxes it
//! and leaves a `Boolean` behind; in CONDITION position it FUSES `instanceof` with the branch it
//! feeds, and that shape did not box — so `if (n is Number)` put an `int` where the verifier wants
//! an object and the class was rejected with `VerifyError: Bad type on operand stack`. The check is
//! the same question either way, so both box.
//!
//! The reference compiler supplies every expectation: these are shapes where a confident reading is
//! exactly what was wrong.

use super::common;

/// Run `body` under krusty AND under the reference compiler, and require the SAME output.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    assert_eq!(krusty, "OK", "{stem}: unexpected krusty result");
    let reference = common::kotlinc_box_result(body);
    assert_eq!(reference, "OK", "{stem}: unexpected kotlinc result");
    assert_eq!(krusty, reference, "{stem}: krusty and kotlinc disagree");
}

/// The FUSED shape: the check is a condition, so `instanceof` feeds the branch directly. This is
/// the one that emitted an unboxed `int` and was rejected by the verifier.
#[test]
fn a_scalar_is_check_in_a_condition_boxes_its_operand() {
    agrees_with_kotlinc(
        "ScalarIsInCondition",
        "fun box(): String {\n\
         \x20   val n = 5\n\
         \x20   if (n is Number) { } else return \"fail: an Int is no Number\"\n\
         \x20   if (n !is Number) return \"fail: !is disagrees\"\n\
         \x20   val d = 1.5\n\
         \x20   if (d is Comparable<Double>) { } else return \"fail: a Double is no Comparable\"\n\
         \x20   val c = 'a'\n\
         \x20   if (c is Comparable<Char>) { } else return \"fail: a Char is no Comparable\"\n\
         \x20   val b = true\n\
         \x20   if (b is Comparable<Boolean>) { } else return \"fail: a Boolean is no Comparable\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

/// The same check in VALUE position, which already boxed — here so that the two shapes are pinned
/// together and cannot drift apart again.
#[test]
fn a_scalar_is_check_as_a_value_answers_the_same() {
    agrees_with_kotlinc(
        "ScalarIsAsValue",
        "fun box(): String {\n\
         \x20   val n = 5\n\
         \x20   val isNumber = n is Number\n\
         \x20   if (!isNumber) return \"fail: an Int is no Number\"\n\
         \x20   val notNumber = n !is Number\n\
         \x20   if (notNumber) return \"fail: !is disagrees\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

/// An UNSIGNED operand boxes as its own type, so it is a `Comparable` of itself and is NOT the
/// signed type it wraps.
#[test]
fn an_unsigned_operand_boxes_as_its_own_type() {
    agrees_with_kotlinc(
        "UnsignedIsCheck",
        "fun box(): String {\n\
         \x20   val u = 1u\n\
         \x20   if (u is Comparable<UInt>) { } else return \"fail: a UInt is no Comparable\"\n\
         \x20   val boxed: Any = u\n\
         \x20   if (boxed is Int) return \"fail: a UInt is an Int\"\n\
         \x20   if (boxed !is UInt) return \"fail: a UInt is no UInt\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

/// The operand is still EVALUATED, in either shape: boxing changes what is asked, not whether the
/// expression runs.
#[test]
fn a_scalar_is_check_still_evaluates_its_operand() {
    agrees_with_kotlinc(
        "ScalarIsEvaluatesOperand",
        "var seen = 0\n\
         fun counted(): Int {\n\
         \x20   seen++\n\
         \x20   return 5\n\
         }\n\
         fun box(): String {\n\
         \x20   if (counted() is Number) { } else return \"fail: an Int is no Number\"\n\
         \x20   val asValue = counted() is Number\n\
         \x20   if (!asValue) return \"fail: the value shape disagrees\"\n\
         \x20   return if (seen == 2) \"OK\" else \"fail: evaluated \" + seen + \" times\"\n\
         }\n",
    );
}
