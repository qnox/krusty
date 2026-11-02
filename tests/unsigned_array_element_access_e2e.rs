//! A subscript on an unsigned array calls the value class's own `get`/`set`, as kotlinc does.
//!
//! `UIntArray` is a value class over `IntArray`, not an array: `xs[i]` is its `get` operator and
//! `xs[i] = v` its `set` operator, which kotlinc emits as the static members
//! `UIntArray.get-pVg5ArA([II)I` and `UIntArray.set-VXSXFK8([III)V` (one mangled pair per width).
//! The carrier's `iaload`/`iastore` would be the same bits, but not kotlinc's class file. Compound
//! assignment, increment and a safe-call receiver reach the same two members.

use super::common;

const FUNCTIONS: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

class Holder(val xs: UIntArray)

fun readU(xs: UIntArray, i: Int): UInt = xs[i]
fun writeU(xs: UIntArray, i: Int, v: UInt) { xs[i] = v }
fun readB(xs: UByteArray, i: Int): UByte = xs[i]
fun writeB(xs: UByteArray, i: Int, v: UByte) { xs[i] = v }
fun readS(xs: UShortArray, i: Int): UShort = xs[i]
fun writeS(xs: UShortArray, i: Int, v: UShort) { xs[i] = v }
fun readL(xs: ULongArray, i: Int): ULong = xs[i]
fun writeL(xs: ULongArray, i: Int, v: ULong) { xs[i] = v }
fun addU(xs: UIntArray, i: Int) { xs[i] += 2u }
fun bumpS(xs: UShortArray, i: Int) { xs[i] = (xs[i] + 1u).toUShort() }
fun postIncU(xs: UIntArray, i: Int): UInt = xs[i]++
fun preDecL(xs: ULongArray, i: Int): ULong = --xs[i]
fun safeRead(h: Holder?, i: Int): UInt? = h?.xs[i]
fun safeWrite(h: Holder?, i: Int, v: UInt) { h?.xs[i] = v }
"#;

const BOX: &str = r#"
fun box(): String {
    val u = UIntArray(3); val b = UByteArray(2); val s = UShortArray(2); val l = ULongArray(2)
    writeU(u, 0, UInt.MAX_VALUE); writeB(b, 1, 255u); writeS(s, 0, 65535u)
    writeL(l, 1, ULong.MAX_VALUE)
    addU(u, 1); addU(u, 1); bumpS(s, 1); bumpS(s, 0)
    val before = postIncU(u, 2)
    val dec = preDecL(l, 0)
    val h = Holder(u)
    safeWrite(h, 2, 9u); safeWrite(null, 0, 1u)
    val texts = listOf(
        readU(u, 0), readU(u, 1), before, readU(u, 2), readB(b, 1), readS(s, 0), readS(s, 1),
        readL(l, 1), dec, safeRead(h, 2), safeRead(null, 0),
    ).joinToString(",")
    val expected = "4294967295,4,0,9,255,0,1,18446744073709551615,18446744073709551615,9,null"
    return if (texts == expected) "OK" else texts
}
"#;

#[test]
fn an_unsigned_array_subscript_calls_its_own_get_and_set_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedElements",
        FUNCTIONS,
        &["UnsignedElementsKt", "Holder"],
    );
}

#[test]
fn an_unsigned_array_subscript_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{FUNCTIONS}{BOX}"),
        "unsigned_array_element_access",
    );
}

/// `Array<UIntArray>` stores each element as its box. A subscript on the element unboxes it once
/// for the class's own `get`/`set` and `size`, and a reference use keeps the box, as kotlinc does.
const NESTED: &str = r#"@file:OptIn(ExperimentalUnsignedTypes::class)

fun nested(): String {
    val back = arrayOf(uintArrayOf(0u, 7u), uintArrayOf(1u, 7u))
    back[1][0] = 5u
    val boxed: Any = back[0]
    return "${back[1][0]} ${back[0][1]} ${back[1].size} ${boxed is UIntArray} ${boxed is IntArray}"
}
"#;

// The compound form on an element stays out of the byte comparison: after the inlined
// `UInt.plus`, kotlinc writes the statement's line again at the `set` call whose receiver is the
// element's temporary, and krusty does not. That line gap is the same for any operator `set` on a
// nested receiver (`Array<C>` with `C.set(Int, UInt)`), not a property of unsigned arrays.
const NESTED_BOX: &str = r#"
fun compound(): String {
    val back = Array(2) { uintArrayOf(it.toUInt(), 7u) }
    back[0][1] += 1u
    back[1][1]--
    return "${back[0][1]} ${back[1][1]}"
}

fun box(): String {
    val texts = listOf(nested(), compound())
    return if (texts == listOf("5 7 2 true false", "8 6")) "OK" else texts.toString()
}
"#;

#[test]
fn an_element_of_an_array_of_unsigned_arrays_is_unboxed_once_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc("UnsignedNested", NESTED, &["UnsignedNestedKt"]);
}

#[test]
fn an_element_of_an_array_of_unsigned_arrays_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{NESTED}{NESTED_BOX}"), "unsigned_nested_arrays");
}
