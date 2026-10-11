//! Kotlin's mixed numeric promotion, run on Native.
//!
//! The result of `Int + Long`, `Char + Int`, `Char - Char` or `Byte * Short` is the result of the
//! operator the checker selected, which common lowering records on the node. Native chooses only
//! the instructions: how each operand widens, and whether the answer narrows back to the recorded
//! type. Each program is first answered by kotlinc on the JVM.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

fn expect_ok_everywhere(source: &str, stem: &str) {
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

#[test]
fn int_and_long_operands_promote_to_long() {
    expect_ok_everywhere(
        "fun box(): String {\n\
         \x20   val i = Int.MAX_VALUE\n\
         \x20   val l = 1L\n\
         \x20   if (i + l != 2147483648L) return \"fail plus: ${i + l}\"\n\
         \x20   if (l - i != -2147483646L) return \"fail minus: ${l - i}\"\n\
         \x20   if (i * 2L != 4294967294L) return \"fail times: ${i * 2L}\"\n\
         \x20   if (7L / 2 != 3L) return \"fail div: ${7L / 2}\"\n\
         \x20   if (-7 % 3L != -1L) return \"fail rem: ${-7 % 3L}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MixedIntLong",
    );
}

#[test]
fn char_arithmetic_keeps_char_except_char_minus_char() {
    expect_ok_everywhere(
        "fun box(): String {\n\
         \x20   val c = 'a'\n\
         \x20   val n = 2\n\
         \x20   if (c + n != 'c') return \"fail plus: ${c + n}\"\n\
         \x20   if (c - 1 != '`') return \"fail minus: ${c - 1}\"\n\
         \x20   val distance: Int = 'z' - c\n\
         \x20   if (distance != 25) return \"fail distance: $distance\"\n\
         \x20   val wrapped = '\\u0000' - 1\n\
         \x20   if (wrapped != '\\uffff') return \"fail wrap: ${wrapped.code}\"\n\
         \x20   if ((wrapped + 1).code != 0) return \"fail unwrap: ${(wrapped + 1).code}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MixedCharArithmetic",
    );
}

#[test]
fn byte_and_short_operands_widen_to_int() {
    expect_ok_everywhere(
        "fun box(): String {\n\
         \x20   val b: Byte = 127\n\
         \x20   val s: Short = 32767\n\
         \x20   val sum = b + b\n\
         \x20   if (sum != 254) return \"fail byte plus: $sum\"\n\
         \x20   val product = s * b\n\
         \x20   if (product != 4161409) return \"fail short times: $product\"\n\
         \x20   val negative: Byte = -128\n\
         \x20   if (negative - 1 != -129) return \"fail byte minus: ${negative - 1}\"\n\
         \x20   if (b + 1L != 128L) return \"fail byte long: ${b + 1L}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MixedNarrowArithmetic",
    );
}

#[test]
fn compound_assignments_keep_the_selected_result() {
    expect_ok_everywhere(
        "fun box(): String {\n\
         \x20   var c = 'x'\n\
         \x20   c += 2\n\
         \x20   if (c != 'z') return \"fail char plus: $c\"\n\
         \x20   c -= 25\n\
         \x20   if (c != 'a') return \"fail char minus: $c\"\n\
         \x20   var l = Long.MAX_VALUE - 1\n\
         \x20   l += 1\n\
         \x20   if (l != Long.MAX_VALUE) return \"fail long plus: $l\"\n\
         \x20   var d = 1.5\n\
         \x20   d *= 3\n\
         \x20   if (d != 4.5) return \"fail double times: $d\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MixedCompoundAssignment",
    );
}

#[test]
fn increments_wrap_at_their_own_type() {
    expect_ok_everywhere(
        "fun box(): String {\n\
         \x20   var c = '\\uffff'\n\
         \x20   c++\n\
         \x20   if (c.code != 0) return \"fail char inc: ${c.code}\"\n\
         \x20   c--\n\
         \x20   if (c.code != 65535) return \"fail char dec: ${c.code}\"\n\
         \x20   var b: Byte = 127\n\
         \x20   b++\n\
         \x20   if (b != (-128).toByte()) return \"fail byte inc: $b\"\n\
         \x20   var s: Short = -32768\n\
         \x20   --s\n\
         \x20   if (s != 32767.toShort()) return \"fail short dec: $s\"\n\
         \x20   var l = Long.MAX_VALUE\n\
         \x20   l++\n\
         \x20   if (l != Long.MIN_VALUE) return \"fail long inc: $l\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MixedIncrements",
    );
}
