//! The flow narrowing a loop's own writes invalidate.
//!
//! A straight-line proof is a proof about ONE edge. A loop has a back edge, so a body that
//! reassigns `x` reaches its own start — and its condition, and the code after the loop — with
//! whatever that assignment left, not with what preceded the loop. What kotlinc rejects here,
//! krusty used to ACCEPT, and then compiled to a cast that failed at run time on both backends
//! (`codegen/box/casts/kt83324.kt`, `codegen/box/objectExpression/expr3.kt`).
//!
//! Every program below was run through kotlinc 2.4.10 first; the accept/reject each asserts is
//! that compiler's answer, not a reading of the rule.
//!
//! The narrowing the loop's own CONDITION proves is the one that must survive, and does: the
//! condition is re-evaluated on every iteration, so it holds on entry to each of them.
//! `a_condition_proof_survives_the_back_edge` is the whole reason the invalidation is written as
//! "clear before the condition is checked" rather than "clear the body scope".
//!
//! Each `var` here is DECLARED and then assigned, rather than initialized in its declaration.
//! That is deliberate: kotlinc narrows an assignment and does not narrow an initializer
//! (`var x: Any = ""` leaves `x.length` unresolved), so writing these the short way would rest
//! them on a separate divergence instead of on the back edge they are about.

use super::common;

fn rejects(src: &str, context: &str) {
    let diagnostics = common::front_end_diagnostics(src, &[], None);
    assert_eq!(
        diagnostics,
        ["unresolved reference 'length'."],
        "{context}: exact diagnostics"
    );
}

fn accepts(src: &str, context: &str) {
    let diagnostics = common::front_end_diagnostics(src, &[], None);
    assert_eq!(diagnostics, Vec::<String>::new(), "{context}");
}

#[test]
fn a_proof_does_not_reach_a_body_that_overwrites_it() {
    rejects(
        "fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = \"\"\n\
         \x20   while (true) {\n\
         \x20       println(x.length)\n\
         \x20       x = 42\n\
         \x20   }\n\
         }\n",
        "a `var` the body reassigns",
    );
}

#[test]
fn a_proof_does_not_reach_the_condition_either() {
    // The condition is on the back edge too, so it is read with whatever the body left.
    rejects(
        "fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = \"\"\n\
         \x20   while (x.length > 0) {\n\
         \x20       x = 42\n\
         \x20   }\n\
         }\n",
        "a condition reading a `var` the body reassigns",
    );
}

#[test]
fn a_do_while_body_is_invalidated_though_its_first_turn_precedes_the_back_edge() {
    // The first iteration really does run before any assignment, and Kotlin still refuses: one
    // reading serves the whole loop, not one per iteration.
    rejects(
        "fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = \"\"\n\
         \x20   do {\n\
         \x20       println(x.length)\n\
         \x20       x = 42\n\
         \x20   } while (true)\n\
         }\n",
        "a do-while body reassigning its own subject",
    );
}

#[test]
fn a_proof_does_not_survive_a_loop_that_overwrites_it() {
    rejects(
        "fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = \"\"\n\
         \x20   for (i in 0..1) { x = 42 }\n\
         \x20   println(x.length)\n\
         }\n",
        "a read after a loop that reassigns",
    );
}

#[test]
fn a_write_only_a_closure_performs_still_invalidates() {
    // The scan descends into lambdas: the write reaches the next iteration however it is spelled.
    rejects(
        "fun execute(block: () -> Unit) { block() }\n\
         fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = \"\"\n\
         \x20   for (i in 0..1) {\n\
         \x20       println(x.length)\n\
         \x20       execute { x = 42 }\n\
         \x20   }\n\
         }\n",
        "a write performed inside a lambda in the body",
    );
}

#[test]
fn a_condition_proof_survives_the_back_edge() {
    // The one that must NOT be cleared. `x` is reassigned in the body and the condition still
    // holds on entry to every iteration, because it is re-evaluated each time.
    accepts(
        "fun f() {\n\
         \x20   var x: String? = \"a\"\n\
         \x20   while (x != null) {\n\
         \x20       println(x.length)\n\
         \x20       x = null\n\
         \x20   }\n\
         }\n",
        "a null check the loop's own condition makes",
    );
}

#[test]
fn a_loop_that_does_not_write_keeps_the_proof() {
    // The invalidation is keyed on the loop's own WRITES, not on there being a loop. Both reads
    // stay resolved: inside the body and after it.
    accepts(
        "fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = \"\"\n\
         \x20   for (i in 0..1) { println(x.length) }\n\
         \x20   println(x.length)\n\
         }\n",
        "a loop reading but not reassigning",
    );
}

#[test]
fn the_proof_still_holds_inside_the_body_after_the_loops_own_write() {
    // Clearing on entry does not disable straight-line narrowing WITHIN one iteration: the
    // assignment itself proves the new type from that point on.
    accepts(
        "fun f() {\n\
         \x20   var x: Any\n\
         \x20   x = 0\n\
         \x20   for (i in 0..1) {\n\
         \x20       x = \"abc\"\n\
         \x20       println(x.length)\n\
         \x20   }\n\
         }\n",
        "a read after the loop's own assignment",
    );
}
