//! `tailrec` through krusty's own code generator.
//!
//! The modifier is a promise about the STACK, not about speed: a function carrying it recurses to a
//! depth chosen because the loop rewrite will remove the recursion, and a backend that emits an
//! ordinary call instead does not answer slowly — it crashes. So the generator needs to know, for
//! each `tailrec`, whether common lowering actually finished the rewrite, and to refuse the ones
//! where it did not rather than discover the answer as a segmentation fault.

use super::common::{expect_native_box, expect_native_decline};

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
fn a_tailrec_still_holding_a_self_call_is_declined() {
    // The rewrite does not descend into a loop — a `continue` to the rewritten loop would leave
    // this one — so the call survives and the function recurses after all. What the generator must
    // not do is emit it: the program would run to a segmentation fault at the depth the source
    // wrote `tailrec` to make safe, and how deep a native stack goes is the machine's business.
    expect_native_decline(
        "tailrec fun walk(n: Int): Int {\n\
         \x20   while (true) {\n\
         \x20       if (n <= 0) return 0\n\
         \x20       return walk(n - 1)\n\
         \x20   }\n\
         }\n\
         fun box(): String = if (walk(1000000) == 0) \"OK\" else \"fail\"\n",
        "UnfinishedTailrec",
        "common lowering leaves recursive",
    );
}

#[test]
fn a_tailrec_with_a_receiver_to_rebind_is_declined() {
    // The rewrite assigns the parameters and jumps; an extension's receiver is not a parameter it
    // can assign, so this one is never attempted and the function recurses as written.
    expect_native_decline(
        "tailrec fun Int.walk(acc: Int): Int = if (this == 0) acc else (this - 1).walk(acc + 1)\n\
         fun box(): String = if (1000000.walk(0) == 1000000) \"OK\" else \"fail\"\n",
        "ExtensionTailrec",
        "common lowering leaves recursive",
    );
}
