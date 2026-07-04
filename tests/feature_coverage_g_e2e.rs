//! End-to-end "box" coverage for the lexer/parser numeric & literal paths (`src/lexer.rs`,
//! `src/parser.rs`) plus the arithmetic/bitwise/conversion emit paths. Each test compiles a
//! `fun box(): String` with `krusty`, runs it on the JVM under `-Xverify:all`, and asserts `OK`.

use super::common;

/// Compile `src` (with entry `box`) under class `stem` and run it, asserting the return is `"OK"`.
/// Skips (returns) when the toolchain env is unavailable — matching the other e2e tests.
fn run(src: &str, stem: &str) {
    common::assert_box_ok_with_stdlib(src, stem);
}

#[test]
fn int_literal_forms() {
    let src = "fun box(): String {\n\
        if (0xFF != 255) return \"hex\"\n\
        if (0b1010 != 10) return \"bin\"\n\
        if (1_000_000 != 1000000) return \"under\"\n\
        if (0xCAFE != 51966) return \"hex2\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "IntLit");
}

#[test]
fn long_literals() {
    let src = "fun box(): String {\n\
        val a: Long = 10L\n\
        val b = 4_000_000_000L\n\
        if (a != 10L) return \"a\"\n\
        if (b != 4000000000L) return \"b\"\n\
        if (0xFFL != 255L) return \"hex\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "LongLit");
}

#[test]
fn negative_literals() {
    let src = "fun box(): String {\n\
        val a = -5\n\
        val b = -3L\n\
        if (a != 0 - 5) return \"a\"\n\
        if (b != 0L - 3L) return \"b\"\n\
        if (-a != 5) return \"neg\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "NegLit");
}

#[test]
fn float_double_literals() {
    let src = "fun box(): String {\n\
        val d = 3.14\n\
        val e = 1.5e3\n\
        val f = 2.0f\n\
        if (d < 3.13 || d > 3.15) return \"d\"\n\
        if (e != 1500.0) return \"e\"\n\
        if (f != 2.0f) return \"f\"\n\
        val sum = d + e\n\
        if (sum < 1503.13 || sum > 1503.15) return \"sum\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "FloatLit");
}

#[test]
fn float_arithmetic_and_compare() {
    let src = "fun box(): String {\n\
        val a = 10.0\n\
        val b = 4.0\n\
        if (a / b != 2.5) return \"div\"\n\
        if (a * b != 40.0) return \"mul\"\n\
        if (!(a > b)) return \"gt\"\n\
        if (a - b != 6.0) return \"sub\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "FloatArith");
}

#[test]
fn char_literals_and_escapes() {
    let src = "fun box(): String {\n\
        if ('a' != 'a') return \"a\"\n\
        if ('\\n'.code != 10) return \"nl\"\n\
        if ('\\t'.code != 9) return \"tab\"\n\
        if ('\\\\'.code != 92) return \"bs\"\n\
        if ('\\''.code != 39) return \"quote\"\n\
        if ('A'.code != 65) return \"code\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "CharLit");
}

#[test]
fn char_arithmetic_and_compare() {
    let src = "fun box(): String {\n\
        val c = 'A'\n\
        if (c + 1 != 'B') return \"inc\"\n\
        if ('z' - 'a' != 25) return \"diff\"\n\
        if (!('a' < 'b')) return \"lt\"\n\
        if ('5' - '0' != 5) return \"digit\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "CharArith");
}

#[test]
fn captured_narrow_incdec_expression() {
    let src = "fun box(): String {\n\
        var b: Byte = 127.toByte()\n\
        fun postB(): Byte = b++\n\
        if (postB() != 127.toByte()) return \"b-post\"\n\
        if (b != (-128).toByte()) return \"b-wrap\"\n\
        fun preB(): Byte = ++b\n\
        if (preB() != (-127).toByte()) return \"b-pre\"\n\
        var s: Short = 32767.toShort()\n\
        fun postS(): Short = s++\n\
        if (postS() != 32767.toShort()) return \"s-post\"\n\
        if (s != (-32768).toShort()) return \"s-wrap\"\n\
        var c: Char = 'A'\n\
        fun postC(): Char = c++\n\
        if (postC() != 'A') return \"c-post\"\n\
        if (c != 'B') return \"c-next\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "CapturedNarrowIncDec");
}

#[test]
fn string_escapes() {
    let src = "fun box(): String {\n\
        if (\"\\n\".length != 1) return \"nl\"\n\
        if (\"\\t\".length != 1) return \"tab\"\n\
        if (\"\\\"quoted\\\"\" != \"\\\"quoted\\\"\") return \"q\"\n\
        if (\"a\\nb\"[1] != '\\n') return \"embed\"\n\
        if (\"\\u0041\" != \"A\") return \"uni\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "StrEsc");
}

#[test]
fn raw_string_trim_indent() {
    let src = "fun box(): String {\n\
        val s = \"\"\"\n\
            line1\n\
            line2\n\
        \"\"\".trimIndent()\n\
        if (s != \"line1\\nline2\") return s\n\
        return \"OK\"\n\
    }\n";
    run(src, "RawStr");
}

#[test]
fn bitwise_int() {
    let src = "fun box(): String {\n\
        if ((0b1100 and 0b1010) != 0b1000) return \"and\"\n\
        if ((0b1100 or 0b1010) != 0b1110) return \"or\"\n\
        if ((0b1100 xor 0b1010) != 0b0110) return \"xor\"\n\
        if ((1 shl 4) != 16) return \"shl\"\n\
        if ((256 shr 2) != 64) return \"shr\"\n\
        if ((-1 ushr 28) != 15) return \"ushr\"\n\
        if (5.inv() != -6) return \"inv\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "BitInt");
}

#[test]
fn bitwise_long() {
    let src = "fun box(): String {\n\
        if ((0b1100L and 0b1010L) != 0b1000L) return \"and\"\n\
        if ((0b1100L or 0b1010L) != 0b1110L) return \"or\"\n\
        if ((0b1100L xor 0b1010L) != 0b0110L) return \"xor\"\n\
        if ((1L shl 40) != 1099511627776L) return \"shl\"\n\
        if ((1024L shr 2) != 256L) return \"shr\"\n\
        if ((-1L ushr 60) != 15L) return \"ushr\"\n\
        if (5L.inv() != -6L) return \"inv\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "BitLong");
}

#[test]
fn int_division_and_remainder() {
    let src = "fun box(): String {\n\
        if (7 / 2 != 3) return \"div\"\n\
        if (7 % 2 != 1) return \"rem\"\n\
        if (-7 / 2 != -3) return \"ndiv\"\n\
        if (-7 % 2 != -1) return \"nrem\"\n\
        if (7 % -2 != 1) return \"rem2\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "IntDiv");
}

#[test]
fn integer_overflow_wrap() {
    let src = "fun box(): String {\n\
        if (Int.MAX_VALUE + 1 != Int.MIN_VALUE) return \"imax\"\n\
        if (Int.MIN_VALUE - 1 != Int.MAX_VALUE) return \"imin\"\n\
        if (Long.MAX_VALUE + 1L != Long.MIN_VALUE) return \"lmax\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "Overflow");
}

#[test]
fn boolean_ops_precedence() {
    let src = "fun box(): String {\n\
        val t = true\n\
        val f = false\n\
        if (!(t || f && f)) return \"prec\"\n\
        if (t && f) return \"and\"\n\
        if (!(!f)) return \"not\"\n\
        if ((t || f) != true) return \"or\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "BoolOps");
}

#[test]
fn increment_decrement() {
    let src = "fun box(): String {\n\
        var i = 5\n\
        if (i++ != 5) return \"post\"\n\
        if (i != 6) return \"postval\"\n\
        if (++i != 7) return \"pre\"\n\
        if (i-- != 7) return \"postdec\"\n\
        if (--i != 5) return \"predec\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "IncDec");
}

#[test]
fn type_conversions() {
    let src = "fun box(): String {\n\
        val i = 65\n\
        if (i.toLong() != 65L) return \"toLong\"\n\
        if (i.toDouble() != 65.0) return \"toDouble\"\n\
        if (i.toChar() != 'A') return \"toChar\"\n\
        if (i.toByte() != 65.toByte()) return \"toByte\"\n\
        if (300L.toInt() != 300) return \"toInt\"\n\
        if (3.9.toInt() != 3) return \"dToInt\"\n\
        if (300.toByte().toInt() != 44) return \"byteWrap\"\n\
        return \"OK\"\n\
    }\n";
    run(src, "Convert");
}
