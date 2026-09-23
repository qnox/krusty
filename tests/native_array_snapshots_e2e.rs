//! `xs.toList()` and `xs.reversed()` on an ARRAY: a snapshot of its elements as a list.
//!
//! A snapshot rather than a view is Kotlin's own answer, and it is observable — writing through
//! the array afterwards leaves the list as it was, which is what the corpus's
//! `arrays/forInReversed/reversedOriginalUpdatedInLoopBody.kt` is written to check.
//!
//! The elements are boxed on the way in, because a list holds references and a primitive array
//! does not: `IntArray.toList()` answering a `List<Int>` is exactly that boxing. Which box each
//! element gets is read from the ARRAY's descriptor, the only thing that knows how wide an element
//! is and how to read its bits — a `LongArray` and a `DoubleArray` share a width and differ only
//! there, and an unsigned array holds the signed one's bits with a different box.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// Both names, over a reference array and a primitive one, with the snapshot pinned.
#[test]
fn an_array_answers_its_elements_as_a_list_and_as_a_reversed_one() {
    let source = "fun box(): String {\n\
         \x20   val ints = intArrayOf(1, 2, 3)\n\
         \x20   val listed = ints.toList()\n\
         \x20   if (listed.size != 3 || listed[0] != 1 || listed[2] != 3) return \"fail toList\"\n\
         \x20   val reversed = ints.reversed()\n\
         \x20   if (reversed[0] != 3 || reversed[2] != 1) return \"fail reversed\"\n\
         \x20   // A snapshot: writing through the array leaves both lists as they were.\n\
         \x20   ints[0] = 99\n\
         \x20   if (listed[0] != 1 || reversed[2] != 1) return \"fail snapshot\"\n\
         \x20   val objects = arrayOf(\"a\", \"b\", \"c\")\n\
         \x20   if (objects.toList()[1] != \"b\") return \"fail object toList\"\n\
         \x20   if (objects.reversed()[0] != \"c\") return \"fail object reversed\"\n\
         \x20   if (intArrayOf().toList().size != 0) return \"fail empty\"\n\
         \x20   var walked = \"\"\n\
         \x20   for (c in charArrayOf('a', 'b', 'c').reversed()) walked += c\n\
         \x20   if (walked != \"cba\") return \"fail walked: \" + walked\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ArraySnapshots");
    expect_native_box(source, "ArraySnapshots", "OK");
}

/// Each element keeps the type its array declares, which is what the BOX decides.
///
/// A `LongArray` and a `DoubleArray` share an element width, and an unsigned array holds the
/// signed one's bits: reading the width alone would answer `4294967295u` as `-1`.
#[test]
fn each_element_is_boxed_as_the_type_its_array_declares() {
    let source = "fun box(): String {\n\
         \x20   if (longArrayOf(1L, 2L).toList().toString() != \"[1, 2]\") return \"fail Long\"\n\
         \x20   if (doubleArrayOf(1.5, 2.5).toList().toString() != \"[1.5, 2.5]\") return \"fail Double\"\n\
         \x20   if (booleanArrayOf(true, false).toList().toString() != \"[true, false]\") return \"fail Boolean\"\n\
         \x20   if (byteArrayOf(1, 2).toList().toString() != \"[1, 2]\") return \"fail Byte\"\n\
         \x20   if (shortArrayOf(1, 2).toList().toString() != \"[1, 2]\") return \"fail Short\"\n\
         \x20   if (floatArrayOf(1.5f).toList().toString() != \"[1.5]\") return \"fail Float\"\n\
         \x20   if (charArrayOf('x').toList().toString() != \"[x]\") return \"fail Char\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ArraySnapshotBoxes");
    expect_native_box(source, "ArraySnapshotBoxes", "OK");
}

/// An UNSIGNED array holds the signed array's bits, and only the box decides how they read.
///
/// `4294967295u` and `-1` are the same four bytes; reading the element width alone would answer
/// the second. Native only, and deliberately: krusty's JVM backend rejects this program with
/// `VerifyError: Type '[I' is not assignable to 'java/lang/Iterable'` — a `UIntArray` IS a
/// `Collection<UInt>` in Kotlin, and that backend hands the erased `int[]` to `Iterable.toList()`
/// without the wrapper. That is a defect of its own, recorded in `docs/SPEC.md`; the expectation
/// here is kotlinc's, taken by running the same program under it.
#[test]
fn an_unsigned_array_reads_its_elements_unsigned() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val unsigned = uintArrayOf(4294967295u, 1u)\n\
         \x20   val listed = unsigned.toList()\n\
         \x20   if (listed.toString() != \"[4294967295, 1]\") return \"fail UInt: \" + listed\n\
         \x20   if (unsigned.reversed().toString() != \"[1, 4294967295]\") return \"fail reversed\"\n\
         \x20   if (ulongArrayOf(18446744073709551615uL).toList().toString() != \"[18446744073709551615]\")\n\
         \x20       return \"fail ULong\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "UnsignedArraySnapshot",
        "OK",
    );
}
