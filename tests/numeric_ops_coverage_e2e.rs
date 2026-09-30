//! Numeric bytecode-emitter coverage: float/double remainder (`frem`/`drem`), float/double ordered
//! comparison (`fcmpg`/`fcmpl`, `dcmpg`/`dcmpl`), narrowing conversions (`i2b`/`i2c`/`i2s`), unsigned
//! shift-right (`iushr`/`lushr`) and integral bitwise or/xor (`ior`/`lor`/`ixor`/`lxor`). The box
//! corpus reaches the add/sub/mul emitters but never these — operands go through param-taking helpers
//! so the compiler cannot const-fold the operation away and must emit the real opcode.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn float_double_remainder() {
    run_ok(
        "FRem",
        "fun frem(a: Float, b: Float): Float = a % b\n\
         fun drem(a: Double, b: Double): Double = a % b\n\
         fun box(): String {\n\
         if (frem(5.5f, 2.0f) != 1.5f) return \"frem\"\n\
         if (drem(5.5, 2.0) != 1.5) return \"drem\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn float_double_ordered_compare() {
    run_ok(
        "FCmp",
        "fun fless(a: Float, b: Float): Boolean = a < b\n\
         fun fgreater(a: Float, b: Float): Boolean = a > b\n\
         fun dless(a: Double, b: Double): Boolean = a < b\n\
         fun dgreater(a: Double, b: Double): Boolean = a > b\n\
         fun box(): String {\n\
         if (!fless(1.0f, 2.0f)) return \"fless\"\n\
         if (fgreater(1.0f, 2.0f)) return \"fgreater\"\n\
         if (!dless(1.0, 2.0)) return \"dless\"\n\
         if (dgreater(1.0, 2.0)) return \"dgreater\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn int_narrowing_conversions() {
    run_ok(
        "IConv",
        "fun toB(x: Int): Byte = x.toByte()\n\
         fun toC(x: Int): Char = x.toChar()\n\
         fun toS(x: Int): Short = x.toShort()\n\
         fun box(): String {\n\
         if (toB(300).toInt() != 44) return \"i2b\"\n\
         if (toC(65) != 'A') return \"i2c\"\n\
         if (toS(70000).toInt() != 4464) return \"i2s\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn unsigned_shift_right() {
    run_ok(
        "UShr",
        "fun iushr(a: Int, b: Int): Int = a ushr b\n\
         fun lushr(a: Long, b: Int): Long = a ushr b\n\
         fun box(): String {\n\
         if (iushr(-8, 1) != 2147483644) return \"iushr\"\n\
         if (lushr(-8L, 1) != 9223372036854775804L) return \"lushr\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn integral_or_xor() {
    run_ok(
        "OrXor",
        "fun ior(a: Int, b: Int): Int = a or b\n\
         fun ixor(a: Int, b: Int): Int = a xor b\n\
         fun lor(a: Long, b: Long): Long = a or b\n\
         fun lxor(a: Long, b: Long): Long = a xor b\n\
         fun box(): String {\n\
         if (ior(5, 2) != 7) return \"ior\"\n\
         if (ixor(5, 1) != 4) return \"ixor\"\n\
         if (lor(5L, 2L) != 7L) return \"lor\"\n\
         if (lxor(5L, 1L) != 4L) return \"lxor\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn integer_constant_conditional_adapts() {
    run_ok(
        "IntConstCond",
        "fun continuation(v: Long, hasMore: Boolean): Int =\n\
         ((v and 0x7f) or if (hasMore) 0x80 else 0).toInt()\n\
         fun whenBits(v: Long, n: Int): Long = v or when (n) { 0 -> 0x80; else -> 0 }\n\
         fun nested(v: Long, a: Boolean, b: Boolean): Long =\n\
         v or if (a) if (b) 0x80 else 0x40 else 0\n\
         fun blocked(v: Long, hasMore: Boolean): Long =\n\
         v or if (hasMore) { 0x80 } else { val ignored = 1; 0 }\n\
         fun shifted(v: Long, wide: Boolean): Long = v shl if (wide) 1 else 2\n\
         fun byteIf(c: Boolean): Byte = if (c) 1 else 2\n\
         fun preferInt(x: Int): String = \"int\"\n\
         fun preferInt(x: Long): String = \"long\"\n\
         fun box(): String {\n\
         if (continuation(1L, true) != 129) return \"cont\"\n\
         if (continuation(1L, false) != 1) return \"cont0\"\n\
         if (whenBits(1L, 0) != 129L) return \"when\"\n\
         if (whenBits(1L, 1) != 1L) return \"when0\"\n\
         if (nested(1L, true, true) != 129L) return \"nest\"\n\
         if (nested(1L, true, false) != 65L) return \"nest2\"\n\
         if (nested(1L, false, true) != 1L) return \"nest0\"\n\
         if (blocked(1L, true) != 129L) return \"block\"\n\
         if (blocked(1L, false) != 1L) return \"block0\"\n\
         if (shifted(1L, true) != 2L) return \"shl\"\n\
         if (shifted(1L, false) != 4L) return \"shl0\"\n\
         if (byteIf(true) != 1.toByte()) return \"byte\"\n\
         if (preferInt(if (true) 1 else 2) != \"int\") return \"prefer\"\n\
         return \"OK\"\n\
         }\n",
    );
}
