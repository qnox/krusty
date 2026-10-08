//! `for (j in break downTo 1u)`, `x in 1u..return` — a range operand that TRANSFERS CONTROL.
//!
//! `break`, `continue`, `return` and `throw` are expressions of type `Nothing`, and Kotlin admits
//! one wherever a value is expected. Evaluating such a bound leaves the lowering: the loop or the
//! test that would have used it is unreachable, so there is nothing left to emit and nothing to
//! decline. What the bound's own effect does — leaving the enclosing loop, returning, throwing —
//! is the whole of what the program observes.
//!
//! A MEMBERSHIP test — `x in 1..break` — is the same question, and the generator answers it the
//! same way, but no test here reaches that path: krusty's frontend does not yet type a range whose
//! bound is `Nothing`, reporting that `contains` cannot be applied, whichever spelling of the
//! transfer is written. The loop cases below are what exercise the rule.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// `break` as the FIRST bound: the outer loop is left before the inner one is built.
#[test]
fn a_break_as_a_range_bound_leaves_the_enclosing_loop() {
    let source = "fun box(): String {\n\
         \x20   var ran = 0\n\
         \x20   for (i in 0..8) {\n\
         \x20       ran++\n\
         \x20       for (j in break downTo 1) {}\n\
         \x20   }\n\
         \x20   return if (ran == 1) \"OK\" else \"fail \" + ran\n\
         }\n";
    every_backend_agrees_with_kotlinc("BreakAsRangeBound", source);
}

/// `continue` as the SECOND bound: the first is evaluated, then control leaves.
#[test]
fn a_continue_as_a_range_bound_goes_round_again() {
    let source = "fun box(): String {\n\
         \x20   var ran = 0\n\
         \x20   for (i in 1..3) {\n\
         \x20       ran++\n\
         \x20       for (j in 1..continue) {}\n\
         \x20       ran += 100\n\
         \x20   }\n\
         \x20   return if (ran == 3) \"OK\" else \"fail \" + ran\n\
         }\n";
    every_backend_agrees_with_kotlinc("ContinueAsRangeBound", source);
}

/// `return` as a bound, answering the function's own result.
#[test]
fn a_return_as_a_range_bound_answers_the_function() {
    let source = "fun box(): String {\n\
         \x20   for (i in 1..return \"OK\") {}\n\
         }\n";
    every_backend_agrees_with_kotlinc("ReturnAsRangeBound", source);
}

/// `throw` as a bound, caught outside the loop.
#[test]
fn a_throw_as_a_range_bound_raises_from_where_it_stands() {
    let source = "fun box(): String {\n\
         \x20   try {\n\
         \x20       for (i in 0..1) {\n\
         \x20           for (j in (throw IllegalStateException(\"OK\")) downTo 1) {}\n\
         \x20       }\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       return e.message ?: \"fail null\"\n\
         \x20   }\n\
         \x20   return \"fail no throw\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("ThrowAsRangeBound", source);
}

/// An UNSIGNED counter, which is the shape the corpus case uses.
#[test]
fn a_control_transfer_bounds_an_unsigned_range_too() {
    let source = "fun box(): String {\n\
         \x20   var ran = 0\n\
         \x20   for (i in 1..2) {\n\
         \x20       ran++\n\
         \x20       for (j in 1u..break) {}\n\
         \x20   }\n\
         \x20   return if (ran == 1) \"OK\" else \"fail \" + ran\n\
         }\n";
    every_backend_agrees_with_kotlinc("UnsignedControlTransferBound", source);
}
