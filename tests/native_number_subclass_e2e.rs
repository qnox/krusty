//! A source class extending `kotlin.Number`.
//!
//! `Number` is the other base the runtime owns that a program may extend. It carries NO state —
//! every member it declares is an abstract conversion — so a subclass of it is laid out exactly as a
//! subclass of `kotlin.Any` is, and it contributes `Any`'s own three slots. The descriptor exists
//! already: a boxed primitive points at it so that `is Number` has something to compare, and a
//! subclass of the program's now points at the same one.
//!
//! A call through a `Number` RECEIVER is the separate question, and the one that made relaxing this
//! unsafe before: the runtime answers `toDouble` out of the boxed-primitive tables, which know
//! nothing of a program's object. Every conversion takes no arguments, so the call site tests the
//! receiver against each class of this file that could stand behind the type and dispatches on that
//! class's own slot, falling through to the runtime otherwise.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The conversions reached through the CLASS, which is what the corpus's `numberToChar` cases do.
#[test]
fn a_number_subclass_answers_its_own_conversions() {
    let source = "class MyNumber(val value: Int) : Number() {\n\
         \x20   override fun toInt(): Int = value\n\
         \x20   override fun toByte(): Byte = toInt().toByte()\n\
         \x20   override fun toDouble(): Double = toInt().toDouble()\n\
         \x20   override fun toFloat(): Float = toInt().toFloat()\n\
         \x20   override fun toLong(): Long = toInt().toLong()\n\
         \x20   override fun toShort(): Short = toInt().toShort()\n\
         }\n\
         fun box(): String {\n\
         \x20   val x = MyNumber('*'.code).toInt().toChar()\n\
         \x20   return if (x == '*') \"OK\" else \"Fail: \" + x\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NumberSubclassOwnType");
    expect_native_box(source, "NumberSubclassOwnType", "OK");
}

/// A subclass IS a `Number`, and is not one of the boxed primitives that also are.
#[test]
fn a_number_subclass_is_a_number_and_no_boxed_primitive() {
    let source = "class MyNumber(val value: Int) : Number() {\n\
         \x20   override fun toInt(): Int = value\n\
         \x20   override fun toByte(): Byte = toInt().toByte()\n\
         \x20   override fun toDouble(): Double = toInt().toDouble()\n\
         \x20   override fun toFloat(): Float = toInt().toFloat()\n\
         \x20   override fun toLong(): Long = toInt().toLong()\n\
         \x20   override fun toShort(): Short = toInt().toShort()\n\
         }\n\
         fun box(): String {\n\
         \x20   val mine: Any = MyNumber(7)\n\
         \x20   if (mine !is Number) return \"fail is Number\"\n\
         \x20   if (mine is Int) return \"fail is Int\"\n\
         \x20   val boxed: Any = 7\n\
         \x20   if (boxed !is Number) return \"fail a box is no Number\"\n\
         \x20   if (boxed is MyNumber) return \"fail a box is mine\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NumberSubclassIsNumber");
    expect_native_box(source, "NumberSubclassIsNumber", "OK");
}
