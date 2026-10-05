//! Values whose Kotlin type is `Nothing` but whose producer still returns.
//!
//! `Nothing` promises there is no value, and most producers keep that promise by never coming
//! back. Two do not. A call whose generic result is SUBSTITUTED to `Nothing` really does produce
//! something — the substitution is erased at the call, so the machine hands back whatever the body
//! returned — and kotlinc lets execution carry on with it. A call that genuinely answers `Nothing`
//! has no continuation at all, and a path that reaches past one is a callee that lied.
//!
//! Common lowering tells the two apart once, from the producer rather than from its spelling, and
//! records the answer on the node. These programs are the observable difference: the first group
//! must run the statement after the bottom value, and the second must not need one.

use super::common::expect_native_box;

#[test]
fn a_substituted_generic_bottom_value_runs_and_falls_through() {
    // `materialize()` is inferred at `Nothing` because the lambda's result is coerced to `Unit`.
    // Its body runs — the appended "K" is the proof — and `test` returns normally afterwards.
    expect_native_box(
        "var result = \"fail\"\n\
         fun <K> materialize(): K {\n\
         \x20   result += \"K\"\n\
         \x20   return \"str\" as K\n\
         }\n\
         fun test(n: Number) {\n\
         \x20   n.let {\n\
         \x20       materialize()\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   result = \"O\"\n\
         \x20   test(42)\n\
         \x20   return result\n\
         }\n",
        "SubstitutedBottomFallsThrough",
        "OK",
    );
}

#[test]
fn a_substituted_generic_bottom_value_hands_on_the_value_it_made() {
    // The same shape read as a value rather than discarded: what reaches the caller is the erased
    // result the body actually returned, which is the only value there is to hand on.
    expect_native_box(
        "fun <K> materialize(): K = \"OK\" as K\n\
         fun <T> myRun(action: () -> T): T = action()\n\
         fun produce(): Any = myRun { materialize() } ?: \"fail\"\n\
         fun box(): String = produce().toString()\n",
        "SubstitutedBottomHandsOnValue",
        "OK",
    );
}

#[test]
fn a_genuinely_divergent_call_ends_its_path() {
    // `stop` never returns, so the statement after it is unreachable and the join that follows has
    // only the other branch to merge. A backend that let the path continue would need a value of
    // type `Nothing` to continue with, and there is none.
    expect_native_box(
        "fun stop(): Nothing {\n\
         \x20   while (true) {\n\
         \x20   }\n\
         }\n\
         fun pick(divergent: Boolean): String {\n\
         \x20   if (divergent) stop()\n\
         \x20   return \"OK\"\n\
         }\n\
         fun box(): String = pick(false)\n",
        "DivergentCallEndsPath",
        "OK",
    );
}

#[test]
fn a_divergent_call_is_a_value_the_other_branch_supplies() {
    // `?:` and a `when` arm read `stop()` in VALUE position. Neither evaluates it here, and the
    // type of the whole expression is the other branch's.
    expect_native_box(
        "fun stop(): Nothing {\n\
         \x20   while (true) {\n\
         \x20   }\n\
         }\n\
         fun named(value: String?): String = value ?: stop()\n\
         fun chosen(flag: Boolean): String = if (flag) stop() else \"K\"\n\
         fun box(): String = named(\"O\") + chosen(false)\n",
        "DivergentCallInValuePosition",
        "OK",
    );
}

#[test]
fn a_loop_that_is_never_false_is_left_only_by_a_break() {
    // The rule the divergent cases above rest on, read from both sides: an unbroken `while (true)`
    // is not left, so a function whose body is one needs no `return` and the end of it is not a
    // position; one with a `break` IS left, so the statement after it runs.
    expect_native_box(
        "fun counted(): String {\n\
         \x20   var at = 0\n\
         \x20   while (true) {\n\
         \x20       at += 1\n\
         \x20       if (at == 3) return \"O\"\n\
         \x20   }\n\
         }\n\
         fun broken(): String {\n\
         \x20   var at = 0\n\
         \x20   while (true) {\n\
         \x20       at += 1\n\
         \x20       if (at == 2) break\n\
         \x20   }\n\
         \x20   return if (at == 2) \"K\" else \"fail\"\n\
         }\n\
         fun box(): String = counted() + broken()\n",
        "AlwaysTrueLoop",
        "OK",
    );
}
