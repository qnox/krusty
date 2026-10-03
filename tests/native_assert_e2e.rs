//! Kotlin's `assert(value)` and `assert(value) { message }`.
//!
//! An INTRINSIC rather than a call, and the mode is why: it decides before anything is evaluated.
//! `always-disable` evaluates NEITHER child, so a condition with a side effect does not have it —
//! which is observable, and the corpus asks it directly.
//!
//! Enabled, the condition is evaluated and branched on. The message ARGUMENT is an ordinary
//! argument and is evaluated with the others, whether or not the assertion holds; what
//! `lazyMessage` makes lazy is the INVOCATION, which happens beside the failure and nowhere else.
//! Getting that backwards is a program a test can see, and this runtime got it backwards once —
//! the two-sided conformance gate said so.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// Enabled: a passing assertion runs its condition and no message, a failing one raises.
#[test]
fn an_enabled_assertion_raises_only_when_its_condition_is_false() {
    let source = "// ASSERTIONS_MODE: always-enable\n\
         var conditions = 0\n\
         fun holds(value: Boolean): Boolean {\n\
         \x20   conditions++\n\
         \x20   return value\n\
         }\n\
         fun box(): String {\n\
         \x20   assert(holds(true))\n\
         \x20   if (conditions != 1) return \"fail passing \" + conditions\n\
         \x20   try {\n\
         \x20       assert(holds(false))\n\
         \x20       return \"fail no raise\"\n\
         \x20   } catch (ignored: AssertionError) {\n\
         \x20   }\n\
         \x20   return if (conditions == 2) \"OK\" else \"fail count \" + conditions\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AssertEnabled");
    expect_native_box(source, "AssertEnabled", "OK");
}

/// The message ARGUMENT is evaluated either way; only its INVOCATION waits for the failure.
///
/// NATIVE only, and deliberately not cross-checked against the JVM: there `assert` is an `inline`
/// function whose whole body — the argument evaluation included — sits inside the
/// `$assertionsDisabled` guard, so the argument is not built at all. Kotlin's own test data says
/// the same by marking the case `TARGET_BACKEND: NATIVE`
/// (`codegen/box/assert/assertEnabledWithFunctionReference.kt`), and that case is the reference
/// this expectation comes from.
#[test]
fn an_assertion_message_is_built_eagerly_and_invoked_lazily() {
    let source = "// ASSERTIONS_MODE: always-enable\n\
         var built = 0\n\
         var invoked = 0\n\
         class Message {\n\
         \x20   fun text(): String {\n\
         \x20       invoked++\n\
         \x20       return \"failed\"\n\
         \x20   }\n\
         }\n\
         fun message(): Message {\n\
         \x20   built++\n\
         \x20   return Message()\n\
         }\n\
         fun box(): String {\n\
         \x20   assert(true, message()::text)\n\
         \x20   if (built != 1) return \"fail not built \" + built\n\
         \x20   if (invoked != 0) return \"fail invoked early \" + invoked\n\
         \x20   try {\n\
         \x20       assert(false, message()::text)\n\
         \x20   } catch (ignored: AssertionError) {\n\
         \x20   }\n\
         \x20   if (built != 2) return \"fail second build \" + built\n\
         \x20   return if (invoked == 1) \"OK\" else \"fail invoked \" + invoked\n\
         }\n";
    expect_native_box(source, "AssertMessageEager", "OK");
}

/// DISABLED: neither child is evaluated, so a condition with a side effect does not have it.
#[test]
fn a_disabled_assertion_evaluates_neither_child() {
    let source = "// ASSERTIONS_MODE: always-disable\n\
         var touched = 0\n\
         fun holds(value: Boolean): Boolean {\n\
         \x20   touched++\n\
         \x20   return value\n\
         }\n\
         fun message(): String {\n\
         \x20   touched++\n\
         \x20   return \"failed\"\n\
         }\n\
         fun box(): String {\n\
         \x20   assert(holds(false))\n\
         \x20   assert(holds(false)) { message() }\n\
         \x20   return if (touched == 0) \"OK\" else \"fail touched \" + touched\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AssertDisabled");
    expect_native_box(source, "AssertDisabled", "OK");
}
