//! `var x: T by Delegates.notNull()` — the delegate whose whole content is a check.
//!
//! One reference and the rule that reading it before it is written is an error, which is Kotlin's
//! `NotNullVar`. `null` is not a value it can hold — its `T` is non-null by declaration — so the
//! empty slot needs no flag beside it the way a lazy's does.
//!
//! The error NAMES the property, as Kotlin's does. The runtime cannot ask the `KProperty` operand
//! for its name (a property reference answers that from a table of the emitted code's own), so the
//! name is a literal the call site takes from the reference's declaration; a `KProperty` this file
//! did not build has no name to take and the call declines rather than reporting a wrong one.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// A member property, written in the constructor and read afterwards.
#[test]
fn a_not_null_delegate_holds_what_was_written_to_it() {
    let source = "class My {\n\
         \x20   var delegate: String by kotlin.properties.Delegates.notNull()\n\
         \x20       private set\n\
         \x20   init {\n\
         \x20       delegate = \"OK\"\n\
         \x20   }\n\
         }\n\
         fun box(): String = My().delegate\n";
    expect_box_ok_with_stdlib(source, "NotNullMember");
    expect_native_box(source, "NotNullMember", "OK");
}

/// A LOCAL delegated variable, captured by an object that reads it after it was written.
#[test]
fn a_local_not_null_delegate_is_read_through_a_capture() {
    let source = "import kotlin.properties.Delegates.notNull\n\
         fun box(): String {\n\
         \x20   var bunny by notNull<String>()\n\
         \x20   val obj = object {\n\
         \x20       val getBunny = { bunny }\n\
         \x20   }\n\
         \x20   bunny = \"OK\"\n\
         \x20   return obj.getBunny()\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NotNullLocal");
    expect_native_box(source, "NotNullLocal", "OK");
}

/// Written twice: the second write is what a read afterwards sees.
#[test]
fn a_not_null_delegate_is_rewritten() {
    let source = "import kotlin.properties.Delegates\n\
         var value: String by Delegates.notNull()\n\
         fun box(): String {\n\
         \x20   value = \"fail\"\n\
         \x20   value = \"OK\"\n\
         \x20   return value\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NotNullRewritten");
    expect_native_box(source, "NotNullRewritten", "OK");
}

/// Read before it was written: `IllegalStateException`, naming the property, as Kotlin's does.
#[test]
fn a_not_null_delegate_read_before_it_was_written_raises() {
    let source = "import kotlin.properties.Delegates\n\
         var value: String by Delegates.notNull()\n\
         fun box(): String {\n\
         \x20   try {\n\
         \x20       return \"fail \" + value\n\
         \x20   } catch (e: IllegalStateException) {\n\
         \x20       return if (e.message == \"Property value should be initialized before get.\")\n\
         \x20           \"OK\"\n\
         \x20       else\n\
         \x20           \"fail message \" + e.message\n\
         \x20   }\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NotNullUnset");
    expect_native_box(source, "NotNullUnset", "OK");
}
