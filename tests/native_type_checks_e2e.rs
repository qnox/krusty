//! `is`, `as` and `as?` against an ARRAY type, through krusty's own code generator and runtime.
//!
//! An array is not a class a program declares, so the generator has no class id to look a
//! descriptor up by — and without a descriptor there is nothing to ask the runtime. But the
//! descriptor exists: every array the generator allocates already wears one of the runtime's own
//! (`kt_type_int_array`, `kt_type_array`, …), because the collector has to be told the element
//! width and whether to look inside. Naming that same descriptor at a type check is the whole of
//! what these programs need.
//!
//! What an array's descriptor distinguishes is the element WIDTH, not the element type a program
//! wrote, which is exactly Kotlin's own erasure: `is Array<String>` is not something a program may
//! write, `is Array<*>` is, and `IntArray` and `Array<Int>` are different types on both sides.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

#[test]
fn a_primitive_array_is_only_its_own_kind() {
    // Each primitive array is a type of its own — `IntArray` is not `LongArray` and neither is an
    // `Array<*>`. Asking through an `Any` is what makes the question the object's rather than the
    // site's: a statically typed receiver could be answered without the runtime at all.
    let src = "fun box(): String {\n\
               \x20   val value: Any = intArrayOf(1, 2, 3)\n\
               \x20   if (value !is IntArray) return \"fail: not an IntArray\"\n\
               \x20   if (value is LongArray) return \"fail: a LongArray\"\n\
               \x20   if (value is CharArray) return \"fail: a CharArray\"\n\
               \x20   if (value is Array<*>) return \"fail: a reference array\"\n\
               \x20   if (value is String) return \"fail: a String\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "PrimitiveArrayIsItsOwnKind");
    expect_native_box(src, "PrimitiveArrayIsItsOwnKind", "OK");
}

#[test]
fn a_reference_array_is_not_a_primitive_one() {
    let src = "fun box(): String {\n\
               \x20   val value: Any = arrayOf(\"a\", \"b\")\n\
               \x20   if (value !is Array<*>) return \"fail: not an Array\"\n\
               \x20   if (value is IntArray) return \"fail: an IntArray\"\n\
               \x20   if (value is ByteArray) return \"fail: a ByteArray\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "ReferenceArrayIsNotPrimitive");
    expect_native_box(src, "ReferenceArrayIsNotPrimitive", "OK");
}

#[test]
fn a_non_array_answers_no_to_every_array_kind() {
    let src = "class Holder\n\
               fun box(): String {\n\
               \x20   val value: Any = Holder()\n\
               \x20   if (value is IntArray) return \"fail: IntArray\"\n\
               \x20   if (value is Array<*>) return \"fail: Array\"\n\
               \x20   if (value is DoubleArray) return \"fail: DoubleArray\"\n\
               \x20   if (value is BooleanArray) return \"fail: BooleanArray\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "NonArrayIsNoArrayKind");
    expect_native_box(src, "NonArrayIsNoArrayKind", "OK");
}

#[test]
fn every_primitive_array_kind_recognizes_itself() {
    // Eight widths share one object shape, and the descriptor is the only thing separating them.
    // One program asks all eight, each through an `Any`, so a single wrong symbol cannot hide
    // behind the seven that are right.
    let src = "fun kind(value: Any): String = when {\n\
               \x20   value is ByteArray -> \"byte\"\n\
               \x20   value is ShortArray -> \"short\"\n\
               \x20   value is IntArray -> \"int\"\n\
               \x20   value is LongArray -> \"long\"\n\
               \x20   value is CharArray -> \"char\"\n\
               \x20   value is BooleanArray -> \"boolean\"\n\
               \x20   value is FloatArray -> \"float\"\n\
               \x20   value is DoubleArray -> \"double\"\n\
               \x20   else -> \"none\"\n\
               }\n\
               fun box(): String {\n\
               \x20   val seen = kind(byteArrayOf(1)) + \" \" + kind(shortArrayOf(1)) + \" \" +\n\
               \x20       kind(intArrayOf(1)) + \" \" + kind(longArrayOf(1)) + \" \" +\n\
               \x20       kind(charArrayOf('a')) + \" \" + kind(booleanArrayOf(true)) + \" \" +\n\
               \x20       kind(floatArrayOf(1.0f)) + \" \" + kind(doubleArrayOf(1.0))\n\
               \x20   val want = \"byte short int long char boolean float double\"\n\
               \x20   return if (seen == want) \"OK\" else \"fail: $seen\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "EveryPrimitiveArrayKind");
    expect_native_box(src, "EveryPrimitiveArrayKind", "OK");
}

#[test]
fn a_nullable_array_check_admits_null() {
    // `x is IntArray?` is true of `null`; `x is IntArray` is not. The runtime answers false for a
    // null receiver, so the nullable form has to say so itself.
    let src = "fun box(): String {\n\
               \x20   val absent: Any? = null\n\
               \x20   if (absent !is IntArray?) return \"fail: null is not IntArray?\"\n\
               \x20   if (absent is IntArray) return \"fail: null is IntArray\"\n\
               \x20   val present: Any? = intArrayOf(1)\n\
               \x20   if (present !is IntArray?) return \"fail: an array is not IntArray?\"\n\
               \x20   return \"OK\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "NullableArrayCheck");
    expect_native_box(src, "NullableArrayCheck", "OK");
}

#[test]
fn a_safe_cast_to_an_array_answers_null_rather_than_the_wrong_array() {
    // `as?` is the check with a value attached, so it has to reach the same descriptor. Answering
    // the receiver unchecked would hand back an `Array<String>` typed `IntArray`, and every read
    // off it would then be four bytes out of an eight-byte slot.
    let src = "fun box(): String {\n\
               \x20   val value: Any = arrayOf(\"a\")\n\
               \x20   if ((value as? IntArray) != null) return \"fail: became an IntArray\"\n\
               \x20   val ints: Any = intArrayOf(7)\n\
               \x20   val back = ints as? IntArray ?: return \"fail: lost its own type\"\n\
               \x20   return if (back[0] == 7) \"OK\" else \"fail: ${back[0]}\"\n\
               }\n";
    expect_box_ok_with_stdlib(src, "SafeCastToArray");
    expect_native_box(src, "SafeCastToArray", "OK");
}
