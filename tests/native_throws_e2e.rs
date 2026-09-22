//! The throws a Kotlin program writes on purpose, through the stdlib rather than with `throw`.
//!
//! `TODO()`, `error(message)` and a failed `require`/`check` all throw in Kotlin, and each builds
//! the exception Kotlin specifies and hands it to the same `kt_throw` a `throw` the program wrote
//! itself takes. That is not tidiness: `error(m)` IS `throw IllegalStateException(m)` in Kotlin, so
//! once a `catch` exists it must not be able to tell them apart, and the surest way to keep that
//! true is for there to be one object and one report rather than two mechanisms.
//!
//! Two halves, and both matter: the path that does not throw must run normally, and the one that
//! does must stop the program, reporting the exception Kotlin names.

use super::common::{expect_native_box, expect_native_decline, expect_native_exit};

#[test]
fn an_unreached_todo_leaves_the_program_alone() {
    expect_native_box(
        "fun pick(n: Int): String = if (n > 0) \"OK\" else TODO()\n\
         fun withReason(n: Int): String = if (n > 0) \"OK\" else TODO(\"not related\")\n\
         fun box(): String = if (pick(1) == \"OK\" && withReason(1) == \"OK\") \"OK\" else \"fail\"\n",
        "UnreachedTodo",
        "OK",
    );
}

#[test]
fn an_unreached_error_leaves_the_program_alone() {
    expect_native_box(
        "fun pick(s: String): String = if (s == \"\") error(\"empty\") else s\n\
         fun box(): String = pick(\"OK\")\n",
        "UnreachedError",
        "OK",
    );
}

#[test]
fn a_satisfied_requirement_is_not_a_failure() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   var checks = 0\n\
         \x20   require(1 > 0)\n\
         \x20   checks++\n\
         \x20   check(2 > 1)\n\
         \x20   checks++\n\
         \x20   return if (checks == 2) \"OK\" else \"fail: $checks\"\n\
         }\n",
        "SatisfiedRequirement",
        "OK",
    );
}

#[test]
fn a_todo_is_a_bottom_value_the_caller_never_reads() {
    // `TODO()` is `Nothing`, so a branch that takes it produces no value at all — an `if` whose
    // other branch does still types, and the whole of the diverging branch is unreachable after.
    expect_native_box(
        "fun describe(n: Int): String {\n\
         \x20   val text: String = if (n > 0) \"positive\" else TODO()\n\
         \x20   return text\n\
         }\n\
         fun box(): String = if (describe(3) == \"positive\") \"OK\" else \"fail\"\n",
        "TodoIsABottomValue",
        "OK",
    );
}

/// A message block that RETURNS from the enclosing function still declines.
///
/// `lazyMessage` is a parameter of an `inline` declaration, so Kotlin lets the block written for it
/// return from the caller. A klib publishes no body to splice, so what arrives is an ordinary
/// function value, and a non-local return through one is a miscompile rather than a slower answer.
///
/// The decline comes from the block itself — the lowering finds no code behind the function value —
/// rather than from a rule about preconditions, which is why this case is pinned here: the
/// ordinary message form beside it IS realized (`tests/native_preconditions_e2e.rs`), so nothing
/// else would notice if this one started being realized too.
#[test]
fn a_message_block_that_returns_non_locally_still_declines() {
    expect_native_decline(
        "fun check(n: Int): String {\n\
         \x20   require(n > 0) { return \"nonlocal\" }\n\
         \x20   return \"ok\"\n\
         }\n\
         fun box(): String = if (check(-1) == \"nonlocal\") \"OK\" else \"fail\"\n",
        "RequireWithANonLocalReturn",
        "no code",
    );
}

#[test]
fn a_reached_todo_reports_kotlins_own_exception() {
    // `NotImplementedError`, with the message the stdlib gives it — not a `krusty:` line of the
    // runtime's own, which is what these reported while there was no `Throwable` to report.
    expect_native_exit(
        "fun box(): String = TODO()\n",
        "ReachedTodo",
        134,
        "kotlin.NotImplementedError: An operation is not implemented.",
    );
}

#[test]
fn a_reached_todo_carries_its_reason() {
    expect_native_exit(
        "fun box(): String = TODO(\"the reason\")\n",
        "ReachedTodoReason",
        134,
        "kotlin.NotImplementedError: An operation is not implemented: the reason",
    );
}

#[test]
fn a_reached_error_is_an_illegal_state_exception() {
    expect_native_exit(
        "fun box(): String = error(\"gone wrong\")\n",
        "ReachedError",
        134,
        "kotlin.IllegalStateException: gone wrong",
    );
}

#[test]
fn a_failed_requirement_is_an_illegal_argument_exception() {
    // Kotlin's two guards differ in the exception they raise, which a `catch` will be able to see:
    // `require` is about the caller's argument and `check` about the receiver's state.
    expect_native_exit(
        "fun box(): String {\n\
         \x20   require(1 > 2)\n\
         \x20   return \"OK\"\n\
         }\n",
        "FailedRequirement",
        134,
        "kotlin.IllegalArgumentException: Failed requirement.",
    );
}

#[test]
fn a_failed_check_is_an_illegal_state_exception() {
    expect_native_exit(
        "fun box(): String {\n\
         \x20   check(1 > 2)\n\
         \x20   return \"OK\"\n\
         }\n",
        "FailedCheck",
        134,
        "kotlin.IllegalStateException: Check failed.",
    );
}

#[test]
fn the_test_assertions_pass_quietly_and_report_kotlins_wording() {
    // The corpus checks itself with these, so the passing path has to cost nothing observable.
    expect_native_box(
        "import kotlin.test.assertEquals\n\
         import kotlin.test.assertTrue\n\
         fun box(): String {\n\
         \x20   assertEquals(3, 1 + 2)\n\
         \x20   assertEquals(\"ab\", \"a\" + \"b\")\n\
         \x20   assertEquals(3, 1 + 2, \"arithmetic\")\n\
         \x20   assertTrue(1 < 2)\n\
         \x20   assertTrue(1 < 2, \"ordering\")\n\
         \x20   return \"OK\"\n\
         }\n",
        "AssertionsPass",
        "OK",
    );
}

/// `assertEquals` compares BOOLEANS structurally, like everything else it compares.
///
/// It is generic, so `assertEquals(true, true)` has `Boolean` as its first parameter after
/// substitution — and reading the parameter to decide how the operands cross handed two raw
/// machine values to an entry point that reads them as references. Only `assertTrue` and
/// `assertFalse` take a `Boolean` as one; the SYMBOL says which, where the parameter cannot.
#[test]
fn the_equality_assertion_compares_booleans_as_values() {
    expect_native_box(
        "import kotlin.test.assertEquals\n\
         fun box(): String {\n\
         \x20   assertEquals(true, 1 < 2)\n\
         \x20   assertEquals(false, 1 > 2)\n\
         \x20   assertEquals(true, true, \"with a message\")\n\
         \x20   return \"OK\"\n\
         }\n",
        "AssertEqualsBooleans",
        "OK",
    );
}

#[test]
fn a_failed_assert_equals_reports_both_values() {
    // Kotlin's wording, which is what a failing assertion has to say for the report to mean the
    // same thing it means under kotlinc.
    expect_native_exit(
        "import kotlin.test.assertEquals\n\
         fun box(): String {\n\
         \x20   assertEquals(3, 4)\n\
         \x20   return \"OK\"\n\
         }\n",
        "AssertEqualsFails",
        134,
        "kotlin.AssertionError: Expected <3>, actual <4>.",
    );
}

#[test]
fn a_failed_assertion_puts_the_callers_message_first() {
    expect_native_exit(
        "import kotlin.test.assertTrue\n\
         fun box(): String {\n\
         \x20   assertTrue(1 > 2, \"ordering\")\n\
         \x20   return \"OK\"\n\
         }\n",
        "AssertTrueFails",
        134,
        "kotlin.AssertionError: ordering. Expected value to be true.",
    );
}
