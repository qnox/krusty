//! `throw` through krusty's own code generator and runtime.
//!
//! A file containing any `try` is declined WHOLE, so inside a file this backend accepts there is no
//! handler and no `finally`. That is what makes terminating at the throw the correct Kotlin answer
//! here rather than a stopgap: an exception nothing handles ends the program, and there is nothing
//! between the throw and the end for the difference to be observable in.
//!
//! `try`/`catch` arrives next, and with it the call-site checks that let a throw reach a handler —
//! see "How an exception propagates" in `docs/BUILD_AND_NATIVE_PLAN.md`.

use super::common::{expect_native_box, expect_native_exit};

#[test]
fn an_uncaught_throw_reports_the_exception_and_ends_the_program() {
    expect_native_exit(
        "fun box(): String {\n\
         \x20   throw RuntimeException(\"boom\")\n\
         }\n",
        "UncaughtThrow",
        134,
        "kotlin.RuntimeException: boom",
    );
}

#[test]
fn a_throw_without_a_message_reports_the_type_alone() {
    expect_native_exit(
        "fun box(): String {\n\
         \x20   throw Exception()\n\
         }\n",
        "ThrowNoMessage",
        134,
        "kotlin.Exception",
    );
}

#[test]
fn a_throw_reached_through_a_call_still_ends_the_program() {
    // The throw is in a callee, so nothing after the call in `box` may run. With no handler
    // anywhere, ending at the throw is exactly that.
    expect_native_exit(
        "fun fail(): Int = throw IllegalStateException(\"inner\")\n\
         fun box(): String {\n\
         \x20   fail()\n\
         \x20   return \"unreachable\"\n\
         }\n",
        "ThrowThroughCall",
        134,
        "kotlin.IllegalStateException: inner",
    );
}

#[test]
fn a_program_that_does_not_throw_is_unaffected() {
    // The control: adding the mechanism must not change a program that never reaches it.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val e = RuntimeException(\"unused\")\n\
         \x20   return if (e.message == \"unused\") \"OK\" else \"fail\"\n\
         }\n",
        "ThrowNotTaken",
        "OK",
    );
}

#[test]
fn an_assertion_error_reports_what_it_was_given() {
    // `AssertionError` is the corpus's own failure signal, and its one-argument form takes `Any?`
    // rather than `String?` — so the message is that value's `toString`, not the value itself.
    expect_native_exit(
        "fun box(): String = throw AssertionError(42)\n",
        "AssertionErrorRendered",
        134,
        "kotlin.AssertionError: 42",
    );
}

#[test]
fn an_assertion_error_without_a_message_names_only_itself() {
    expect_native_exit(
        "fun box(): String = throw AssertionError()\n",
        "AssertionErrorBare",
        134,
        "kotlin.AssertionError",
    );
}

#[test]
fn the_wider_hierarchy_is_chained_the_way_kotlin_chains_it() {
    // Each of these is reachable by name and sits where Kotlin puts it. `NumberFormatException`
    // under `IllegalArgumentException` rather than beside it is the one a `catch` will notice.
    expect_native_exit(
        "fun box(): String = throw NumberFormatException(\"not a number\")\n",
        "NumberFormatThrown",
        134,
        "kotlin.NumberFormatException: not a number",
    );
}

#[test]
fn a_message_that_is_any_renders_where_a_string_message_passes_through() {
    // The difference between the two constructor shapes, made observable: `Exception(null)` has NO
    // message and reports the type alone, while `AssertionError(null)` reports the text `null`,
    // because its parameter is `Any?` and the message is that value's `toString`.
    expect_native_exit(
        "fun box(): String = throw AssertionError(null)\n",
        "AssertionErrorNullMessage",
        134,
        "kotlin.AssertionError: null",
    );
}
