//! A builtin scalar's own `hashCode()` on Native.
//!
//! The provider selects each of the eight scalar `hashCode()` declarations as one primitive-hash
//! operation, and the Native target realizes that operation by the rule Kotlin fixes for the
//! receiver's type. Source calls and calls inside serialized stdlib bodies reach the same
//! realization. Every expectation is kotlinc's: the JVM oracle runs the program first.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// Each scalar through a parameter, so no call folds, at the values where the rules differ:
/// sign extension, the halves of a `Long`, the bits of a floating-point zero and of `NaN`.
#[test]
fn each_builtin_scalar_hashes_by_its_own_rule() {
    let source = "fun byteHash(value: Byte): Int = value.hashCode()\n\
         fun shortHash(value: Short): Int = value.hashCode()\n\
         fun intHash(value: Int): Int = value.hashCode()\n\
         fun longHash(value: Long): Int = value.hashCode()\n\
         fun charHash(value: Char): Int = value.hashCode()\n\
         fun booleanHash(value: Boolean): Int = value.hashCode()\n\
         fun floatHash(value: Float): Int = value.hashCode()\n\
         fun doubleHash(value: Double): Int = value.hashCode()\n\
         fun box(): String {\n\
         \x20   if (byteHash(-17) != -17 || shortHash(-300) != -300) return \"narrow\"\n\
         \x20   if (intHash(-70000) != -70000) return \"int\"\n\
         \x20   if (longHash(4294967297L) != 0 || longHash(4294967298L) != 3) return \"long halves\"\n\
         \x20   if (longHash(-1L) != 0) return \"long sign\"\n\
         \x20   if (charHash('\\u03A9') != 937) return \"char\"\n\
         \x20   if (booleanHash(true) != 1231 || booleanHash(false) != 1237) return \"boolean\"\n\
         \x20   if (floatHash(-0.0f) != (-2147483647 - 1)) return \"float zero\"\n\
         \x20   if (floatHash(0.0f / 0.0f) != 2143289344 || floatHash(1.5f) != 1069547520) return \"float bits\"\n\
         \x20   if (doubleHash(-0.0) != (-2147483647 - 1)) return \"double zero\"\n\
         \x20   if (doubleHash(0.0 / 0.0) != 2146959360 || doubleHash(1.5) != 1073217536) return \"double bits\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "PrimitiveHashCodes");
    expect_native_box(source, "PrimitiveHashCodes", "OK");
}
