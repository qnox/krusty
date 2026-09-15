//! Kotlin's four unsigned arrays on the JVM.
//!
//! `UIntArray` is `@JvmInline value class UIntArray(private val storage: IntArray)`, so at the
//! bytecode level an unsigned array IS the signed array of the same width: `UByteArray` is `byte[]`,
//! `UShortArray` is `short[]`, `UIntArray` is `int[]`, `ULongArray` is `long[]`. The element width
//! decides the allocation, the load and the store opcode, and the array descriptor; only the reading
//! of the bits is unsigned, and that happens above the array.

use super::common;

/// Run `body` under krusty AND under the reference compiler, and require the same output.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    assert_eq!(krusty, common::kotlinc_box_result(body), "{stem}");
}

#[test]
fn every_unsigned_width_reads_back_what_it_stored() {
    // Each width's top value is the one a SIGNED read of the same bits answers as -1, so a wrong
    // width or a wrong load opcode cannot answer correctly by accident.
    agrees_with_kotlinc(
        "UnsignedArrayRoundTrip",
        "fun box(): String {\n\
         \x20   val bytes = UByteArray(1)\n\
         \x20   val shorts = UShortArray(1)\n\
         \x20   val ints = UIntArray(1)\n\
         \x20   val longs = ULongArray(1)\n\
         \x20   bytes[0] = 255u\n\
         \x20   shorts[0] = 65535u\n\
         \x20   ints[0] = 4294967295u\n\
         \x20   longs[0] = 18446744073709551615uL\n\
         \x20   val text = \"\" + bytes[0] + \" \" + shorts[0] + \" \" + ints[0] + \" \" + longs[0]\n\
         \x20   return if (text == \"255 65535 4294967295 18446744073709551615\") \"OK\"\n\
         \x20          else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_keeps_its_own_element_width() {
    // `size` and a second element prove the allocation is the right WIDTH rather than merely wide
    // enough: a `UByteArray` laid out as `int[]` still answers element 0 correctly.
    agrees_with_kotlinc(
        "UnsignedArrayWidth",
        "fun box(): String {\n\
         \x20   val bytes = UByteArray(2)\n\
         \x20   bytes[0] = 1u\n\
         \x20   bytes[1] = 255u\n\
         \x20   val shorts = UShortArray(2)\n\
         \x20   shorts[0] = 1u\n\
         \x20   shorts[1] = 65535u\n\
         \x20   val text = \"${bytes.size} ${bytes[0]} ${bytes[1]} ${shorts.size} ${shorts[1]}\"\n\
         \x20   return if (text == \"2 1 255 2 65535\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_literal_carries_its_elements() {
    agrees_with_kotlinc(
        "UnsignedArrayLiteral",
        "fun box(): String {\n\
         \x20   val values = ubyteArrayOf(1u, 255u)\n\
         \x20   val longs = ulongArrayOf(18446744073709551615uL)\n\
         \x20   val text = \"${values[0]} ${values[1]} ${longs[0]}\"\n\
         \x20   return if (text == \"1 255 18446744073709551615\") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_walks_its_elements_unsigned() {
    // Iteration reads through a different path than an indexed read, so a right index and a wrong
    // walk would still print negatives.
    agrees_with_kotlinc(
        "UnsignedArrayWalk",
        "fun box(): String {\n\
         \x20   val values = ubyteArrayOf(255u, 7u)\n\
         \x20   var text = \"\"\n\
         \x20   for (value in values) text += \"$value \"\n\
         \x20   return if (text == \"255 7 \") \"OK\" else \"fail: $text\"\n\
         }\n",
    );
}

#[test]
fn an_unsigned_array_is_the_signed_array_it_is_laid_out_as() {
    // The reference compiler decides this one, not me: a `UIntArray` erases to `int[]`, so on this
    // target the two are the same runtime type. Asserted against kotlinc rather than asserted from
    // first principles, because Kotlin/Native answers the opposite and the erasure is what differs.
    agrees_with_kotlinc(
        "UnsignedArrayErasure",
        "fun box(): String {\n\
         \x20   val unsigned: Any = UIntArray(1)\n\
         \x20   return \"\" + (unsigned is IntArray) + \" \" + (unsigned is LongArray)\n\
         }\n",
    );
}
