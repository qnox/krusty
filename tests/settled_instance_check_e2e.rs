//! `is` against a type whose membership the LANGUAGE settles, not the object.
//!
//! `Nothing` has no instances, so `x is Nothing` is false whatever `x` holds, and `x is Nothing?`
//! admits exactly `null` — `Nothing?` is the type of `null` and of nothing else. Neither question
//! reaches the object, and there is no class to ask about either: `kotlin.Nothing` is uninstantiable
//! by construction, so testing against it would be asking a question whose answer is already known.
//!
//! The emitter used to answer both with `true`. `ref_internal` had no arm for `Ty::Nothing` or
//! `Ty::Unit`, so each fell through to `java/lang/Object` and the emitted `instanceof` was one every
//! non-null value passes.
//!
//! The receiver is still evaluated. A settled check folds the ANSWER, not the expression, and a
//! receiver may have effects.

use super::common;

/// Run `body` under krusty AND under the reference compiler, and require the SAME output.
///
/// Not `expect_box_ok_with_stdlib`: that asserts krusty answers `"OK"`, which pins my reading of
/// the rule rather than kotlinc's. These shapes are exactly the ones where krusty's own answer was
/// confidently wrong, so the reference compiler has to be the one supplying it.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    assert_eq!(krusty, common::kotlinc_box_result(body), "{stem}");
}

#[test]
fn no_value_is_an_instance_of_nothing() {
    agrees_with_kotlinc(
        "NoValueIsNothing",
        "fun box(): String {\n\
         \x20   val present: Any? = \"a\"\n\
         \x20   val absent: Any? = null\n\
         \x20   if (present is Nothing) return \"fail: a value is Nothing\"\n\
         \x20   if (absent is Nothing) return \"fail: null is Nothing\"\n\
         \x20   if (present !is Nothing) { } else return \"fail: !is disagrees\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

#[test]
fn nullable_nothing_admits_null_and_nothing_else() {
    agrees_with_kotlinc(
        "NullableNothingIsNull",
        "fun box(): String {\n\
         \x20   val present: Any? = \"a\"\n\
         \x20   val absent: Any? = null\n\
         \x20   if (present is Nothing?) return \"fail: a value is Nothing?\"\n\
         \x20   if (absent !is Nothing?) return \"fail: null is not Nothing?\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

#[test]
fn unit_is_a_class_a_value_either_is_or_is_not() {
    // Not settled by the language — `Unit` has exactly one instance, so this is a real question
    // about the object. It answered `true` for everything for the same reason `Nothing` did.
    agrees_with_kotlinc(
        "UnitIsARealQuestion",
        "fun box(): String {\n\
         \x20   val unit: Any = Unit\n\
         \x20   val other: Any = \"a\"\n\
         \x20   if (unit !is Unit) return \"fail: Unit is not Unit\"\n\
         \x20   if (other is Unit) return \"fail: a String is Unit\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

#[test]
fn a_settled_check_still_evaluates_its_receiver() {
    agrees_with_kotlinc(
        "SettledCheckEvaluatesReceiver",
        "var calls = 0\n\
         fun subject(): Any? { calls++; return \"a\" }\n\
         fun box(): String {\n\
         \x20   val never = subject() is Nothing\n\
         \x20   val nullOnly = subject() is Nothing?\n\
         \x20   if (never) return \"fail: Nothing\"\n\
         \x20   if (nullOnly) return \"fail: Nothing?\"\n\
         \x20   return if (calls == 2) \"OK\" else \"fail: $calls\"\n\
         }\n",
    );
}

#[test]
fn a_safe_cast_to_nothing_answers_null_and_evaluates_its_subject_once() {
    // The other side of the same rule, and its own path: `as?` lowers to a guard, a checked cast
    // and `null`, and the guard used to erase to `Object` — so a non-null value SURVIVED a cast
    // that can only ever yield `null`. The counter is what makes "once" a claim: the lowering
    // evaluates the subject into a temporary and the guard reads that temporary, so a shape that
    // re-evaluated instead would answer `null` just as well and count two.
    agrees_with_kotlinc(
        "SafeCastToNothing",
        "var calls = 0\n\
         fun subject(): Any? { calls++; return \"a\" }\n\
         fun box(): String {\n\
         \x20   val cast = subject() as? Nothing\n\
         \x20   if (cast != null) return \"fail: survived the cast\"\n\
         \x20   return if (calls == 1) \"OK\" else \"fail: $calls\"\n\
         }\n",
    );
}

#[test]
fn a_safe_cast_to_nullable_nothing_admits_only_null() {
    agrees_with_kotlinc(
        "SafeCastToNullableNothing",
        "fun box(): String {\n\
         \x20   val present: Any? = \"a\"\n\
         \x20   val absent: Any? = null\n\
         \x20   if ((present as? Nothing?) != null) return \"fail: a value survived\"\n\
         \x20   if ((absent as? Nothing?) != null) return \"fail: null became a value\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}
