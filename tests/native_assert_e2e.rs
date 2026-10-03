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
