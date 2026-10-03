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

/// An `object` whose initializer throws is not handed out half-built. The getter publishes the
/// instance before its constructor runs, so a constructor reaching back finds it; a constructor
/// that throws withdraws it again. The JVM answers the second access with `NoClassDefFoundError`
/// and this target by running the initializer again — both throw, which is what the program pins.
#[test]
fn a_throwing_object_initializer_leaves_no_instance_behind() {
    let source = "fun compute(): Int = throw IllegalStateException(\"boom\")\n\
         object O { val x: Int = compute(); fun get() = x }\n\
         fun box(): String {\n\
         \x20   try { O.get(); return \"fail first\" } catch (e: Throwable) {}\n\
         \x20   return try { \"second: ${O.get()}\" } catch (e: Throwable) { \"OK\" }\n\
         }\n";
    super::common::expect_box_ok_with_stdlib(source, "ThrowingObjectInit");
    expect_native_box(source, "ThrowingObjectInit", "OK");
}

/// The same for an enum: a constant whose constructor throws leaves every constant unbuilt.
#[test]
fn a_throwing_enum_constant_leaves_no_constants_behind() {
    let source = "fun compute(): Int = throw IllegalStateException(\"boom\")\n\
         enum class E(val v: Int) { A(compute()), B(1) }\n\
         fun box(): String {\n\
         \x20   try { E.B; return \"fail first\" } catch (e: Throwable) {}\n\
         \x20   return try { \"second: ${E.B}\" } catch (e: Throwable) { \"OK\" }\n\
         }\n";
    super::common::expect_box_ok_with_stdlib(source, "ThrowingEnumInit");
    expect_native_box(source, "ThrowingEnumInit", "OK");
}

/// A top-level initializer that throws ends the program before the entry's first statement: the
/// JVM fails the facade's initializer before `main` runs. The entry here throws an exception of its
/// own first thing, so reaching it would report that one instead of the initializer's.
#[test]
fn a_throwing_top_level_initializer_stops_before_the_entry() {
    expect_native_exit(
        "fun throwing(): Int = throw IllegalStateException(\"boom\")\n\
         val boom: Int = throwing()\n\
         fun box(): String {\n\
         \x20   throw RuntimeException(\"entered\")\n\
         }\n",
        "ThrowingTopLevelInit",
        134,
        "kotlin.IllegalStateException: boom",
    );
}
