//! Narrow signed constants keep their checked identity through common lowering and JVM boxing.

use super::common;

/// The fixture deliberately owns the constants and generic function. A stdlib companion constant
/// or stdlib generic would let a name-specific intrinsic or library lowering hide a broken ordinary
/// constant path.
#[test]
fn repository_constants_keep_byte_and_short_identity_when_boxed() {
    const SOURCE: &str = r#"
        const val TINY_TOKEN: Byte = 77
        const val MEDIUM_TOKEN: Short = 12345

        fun <T> sameOwned(left: T, right: T): Boolean = left == right

        fun box(): String {
            val byteSlot: Byte = TINY_TOKEN
            val shortSlot: Short = MEDIUM_TOKEN
            val boxedByte: Any = TINY_TOKEN
            val boxedShort: Any = MEDIUM_TOKEN

            if (boxedByte !is Byte) return "byte identity"
            if (boxedShort !is Short) return "short identity"
            if (!sameOwned<Any>(TINY_TOKEN, byteSlot)) return "byte equality"
            if (!sameOwned<Any>(MEDIUM_TOKEN, shortSlot)) return "short equality"
            return "OK"
        }
    "#;

    common::expect_box_same_as_kotlinc(SOURCE, "NarrowIntegralConstantIdentity");
}
