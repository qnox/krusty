//! `tailrec` through krusty's own code generator.
//!
//! The modifier is a promise about the STACK, not about speed: a function carrying it recurses to a
//! depth chosen because the loop rewrite will remove the recursion, and a backend that emits an
//! ordinary call instead does not answer slowly — it crashes.
//!
//! The promise covers the TAIL calls and nothing else. A self-call with work after it is not one,
//! and Kotlin says so: it reports NON_TAIL_RECURSIVE_CALL and every Kotlin backend emits the call.
//! So a `tailrec` whose body still holds such a call is an ordinary program, compiled the way
//! kotlinc compiles it — the one shape this generator declines is a `tailrec` Kotlin loops and
//! this lowering does not.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn a_rewritten_tailrec_runs_in_constant_stack() {
    // A million deep: an emitted call would exhaust any stack this target has.
    expect_native_box(
        "tailrec fun count(n: Int, acc: Int): Int = if (n == 0) acc else count(n - 1, acc + 1)\n\
         fun box(): String = if (count(1000000, 0) == 1000000) \"OK\" else \"fail\"\n",
        "RewrittenTailrec",
        "OK",
    );
}

#[test]
fn a_tailrec_holding_a_non_tail_self_call_loops_the_one_that_is_a_tail_call() {
    // `tailrec` is a promise about the tail calls, not about every call in the body: `1 + walk(…)`
    // has work after it and is not one, which Kotlin says too by reporting NON_TAIL_RECURSIVE_CALL
    // for it. The rewrite loops the tail call beside it and correctly leaves this one, so the
    // function both runs flat and recurses — flat down the million, then seven frames deep.
    //
    // This was declined, on the premise that a surviving self-call is a rewrite that did not
    // finish. It is not: kotlinc emits the same call, so declining refused a program kotlinc
    // compiles the same way. The answer is cross-checked against the JVM backend rather than
    // asserted here, because what `walk` answers is a fact about Kotlin and not about a target.
    let source = "tailrec fun walk(n: Int): Int {\n\
         \x20   if (n <= 0) return 0\n\
         \x20   if (n == 7) return 1 + walk(n - 1)\n\
         \x20   return walk(n - 1)\n\
         }\n\
         fun box(): String = if (walk(1000000) == 1) \"OK\" else \"fail \" + walk(1000000)\n";
    expect_box_ok_with_stdlib(source, "NonTailSelfCall");
    expect_native_box(source, "NonTailSelfCall", "OK");
}

#[test]
fn a_tailrec_local_function_runs_flat() {
    // The LOCAL counterpart, and the last of the three to be rewritten. It was declined on the
    // premise that no phase loops a body-local `tailrec` — true when this backend was written, and
    // no longer: common lowering gives a local the same frame a declared one gets, computing its
    // logical parameter count from the slots its captures occupy rather than assuming the list
    // starts at zero.
    //
    // A million deep is the observable, as for the sibling cases: an emitted call would exhaust any
    // stack this target has, so the program answering at all is the assertion.
    expect_native_box(
        "fun box(): String {\n\
         \x20   tailrec fun count(n: Int, acc: Int): Int = if (n == 0) acc else count(n - 1, acc + 1)\n\
         \x20   return if (count(1000000, 0) == 1000000) \"OK\" else \"fail\"\n\
         }\n",
        "LocalTailrec",
        "OK",
    );
}

#[test]
fn a_tailrec_extension_rebinds_its_receiver_and_runs_flat() {
    // This was declined on the premise that an extension's receiver is not a parameter the step can
    // assign. It is one: the receiver is inserted into the parameter list at its own position and
    // passed as an ordinary argument, so the step re-binds it like any other — which is also why
    // the self-call may name a DIFFERENT receiver and still be a loop step. A million deep is the
    // observable, since an emitted call would exhaust any stack this target has.
    expect_native_box(
        "tailrec fun Int.walk(acc: Int): Int = if (this == 0) acc else (this - 1).walk(acc + 1)\n\
         fun box(): String = if (1000000.walk(0) == 1000000) \"OK\" else \"fail\"\n",
        "ExtensionTailrec",
        "OK",
    );
}
