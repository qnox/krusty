//! `const val` initializers fold the builtin bit operations kotlinc's constant evaluator folds:
//! infix `and`, `or` and `xor` on `Int`, `Long` and `Boolean`, and `inv()` on `Int` and `Long`.
//! An unfolded initializer left a private constant as a property read through the object's own
//! `this`, which the backend cannot realize for static storage.

use super::common;

const SOURCE: &str = r#"
interface Strategy {
    fun canMove(flag: Int): Boolean

    object LineOfSight : Strategy {
        private const val MASK = 0x1 or 0x2

        override fun canMove(flag: Int): Boolean = (flag and MASK) == 0
    }
}

object Bits {
    const val AND = 0xF0 and 0x3C
    const val XOR = 0xF0 xor 0x3C
    const val INV = 0x0F.inv()
    const val LONG_AND = 0xF0L and 0x3CL
    const val LONG_OR = 1L or (1L shl 40)
    const val LONG_XOR = -1L xor 5L
    const val LONG_INV = 7L.inv()
    const val BOTH = true and false
    const val EITHER = false or true
    const val ONE = true xor false
    const val MIXED = (1 shl 4) or 3 and 0xFF
    private const val HIDDEN = 0x10 or 0x01

    fun read(value: Int): Int = value and HIDDEN
}

fun box(): String {
    if (!Strategy.LineOfSight.canMove(4)) return "canMove"
    if (Bits.read(0x11) != 0x11) return "read"
    if (Bits.INV != -16 || Bits.LONG_INV != -8L) return "inv"
    return "OK"
}
"#;

#[test]
fn const_bit_operations_fold_like_kotlinc() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "ConstBits"),
        "OK"
    );
    common::assert_classes_identical_to_kotlinc(
        "ConstBits",
        SOURCE,
        &["Strategy", "Strategy$LineOfSight", "Bits", "ConstBitsKt"],
    );
}
