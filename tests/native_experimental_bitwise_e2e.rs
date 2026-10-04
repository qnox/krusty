//! `kotlin.experimental`'s bit operations on the narrow integers.
//!
//! Kotlin gives `Int` and `Long` `and`/`or`/`xor`/`inv` as members and gives `Byte` and `Short`
//! the same four as extensions in `kotlin.experimental`. That is where the library put the
//! declaration, not a difference in what the operation means: the answer is the machine's, at the
//! receiver's own width. So these are instructions rather than a runtime call — the receiver never
//! becomes an object to reach them.
//!
//! The width is what makes them worth pinning. A `Byte` is carried as an 8-bit machine integer and
//! Kotlin's `inv()` answers a `Byte`, so `0x0F.toByte().inv()` is `-16` and not the 32-bit
//! `-16` an `Int` operation would produce before narrowing — the two agree here and would not for
//! a shift, which is why Kotlin declares no shift for these types at all.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The four operations at both widths, against answers taken from kotlinc.
#[test]
fn the_narrow_integers_answer_their_bit_operations_at_their_own_width() {
    let source = "import kotlin.experimental.and\n\
         import kotlin.experimental.inv\n\
         import kotlin.experimental.or\n\
         import kotlin.experimental.xor\n\
         fun box(): String {\n\
         \x20   val b1: Byte = 0xDC.toByte()\n\
         \x20   val b2: Byte = 0x65.toByte()\n\
         \x20   if ((b1 and b2) != 0x44.toByte()) return \"fail Byte.and\"\n\
         \x20   if ((b1 or b2) != 0xFD.toByte()) return \"fail Byte.or\"\n\
         \x20   if ((b1 xor b2) != 0xB9.toByte()) return \"fail Byte.xor\"\n\
         \x20   if (b1.inv() != 0x23.toByte()) return \"fail Byte.inv\"\n\
         \x20   val s1: Short = 0xDC56.toShort()\n\
         \x20   val s2: Short = 0x65DC.toShort()\n\
         \x20   if ((s1 and s2) != 0x4454.toShort()) return \"fail Short.and\"\n\
         \x20   if ((s1 or s2) != 0xFDDE.toShort()) return \"fail Short.or\"\n\
         \x20   if ((s1 xor s2) != 0xB98A.toShort()) return \"fail Short.xor\"\n\
         \x20   if (s1.inv() != 0x23A9.toShort()) return \"fail Short.inv\"\n\
         \x20   // The width is observable: an `inv` at 32 bits would answer -221 here.\n\
         \x20   if (0x0F.toByte().inv().toInt() != -16) return \"fail narrow inv\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "ExperimentalBitwise");
    expect_native_box(source, "ExperimentalBitwise", "OK");
}
