//! End-to-end "box" coverage for value/inline classes and unsigned types. Each test compiles a
//! `fun box(): String` with krusty, runs it on a real JVM under verification, and asserts `"OK"`.
//! Targets `src/jvm/value_classes.rs` and `src/jvm/inline.rs`.

use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics, so callers
/// cannot turn either failure into a passing skip.
fn run(src: &str, stem: &str) -> String {
    common::expect_box_run_with_stdlib(src, stem)
}

#[test]
fn value_class_construct_read() {
    let src = "@JvmInline value class X(val v: Int)\n\
fun box(): String {\n\
    val x = X(7)\n\
    if (x.v != 7) return \"got ${x.v}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcRead");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_arithmetic_and_pass() {
    let src = "@JvmInline value class X(val v: Int)\n\
fun add(a: X, b: X): Int = a.v + b.v\n\
fun box(): String {\n\
    val r = add(X(3), X(4))\n\
    if (r != 7) return \"got $r\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcAdd");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_returned() {
    let src = "@JvmInline value class X(val v: Int)\n\
fun make(n: Int): X = X(n * 2)\n\
fun box(): String {\n\
    val x = make(5)\n\
    if (x.v != 10) return \"got ${x.v}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcRet");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_wrapping_string() {
    let src = "@JvmInline value class Name(val s: String)\n\
fun box(): String {\n\
    val n = Name(\"hi\")\n\
    if (n.s != \"hi\") return \"got ${n.s}\"\n\
    if (n.s.length != 2) return \"len ${n.s.length}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcStr");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_member_function() {
    let src = "@JvmInline value class X(val v: Int) {\n\
    fun doubled(): Int = v * 2\n\
}\n\
fun box(): String {\n\
    val x = X(6)\n\
    if (x.doubled() != 12) return \"got ${x.doubled()}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcMember");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_implements_interface() {
    let src = "interface Named { fun label(): String }\n\
@JvmInline value class Tag(val v: Int) : Named {\n\
    override fun label(): String = \"t$v\"\n\
}\n\
fun box(): String {\n\
    val t: Named = Tag(3)\n\
    if (t.label() != \"t3\") return \"got ${t.label()}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcIface");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_boxed_at_generic_boundary() {
    let src = "@JvmInline value class X(val v: Int)\n\
fun box(): String {\n\
    val list: List<X> = listOf(X(1), X(2), X(3))\n\
    var sum = 0\n\
    for (e in list) sum += e.v\n\
    if (sum != 6) return \"got $sum\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcGeneric");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_nullable() {
    let src = "@JvmInline value class X(val v: Int)\n\
fun pick(b: Boolean): X? = if (b) X(9) else null\n\
fun box(): String {\n\
    val a = pick(true)\n\
    if (a == null) return \"null a\"\n\
    if (a.v != 9) return \"got ${a.v}\"\n\
    val b = pick(false)\n\
    if (b != null) return \"not null b\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcNullable");
    assert_eq!(out, "OK");
}

#[test]
fn value_class_as_map_key() {
    let src = "@JvmInline value class Id(val v: Int)\n\
fun box(): String {\n\
    val m = HashMap<Id, String>()\n\
    m[Id(1)] = \"a\"\n\
    m[Id(2)] = \"b\"\n\
    if (m[Id(1)] != \"a\") return \"got ${m[Id(1)]}\"\n\
    if (m[Id(2)] != \"b\") return \"got ${m[Id(2)]}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "VcMapKey");
    assert_eq!(out, "OK");
}

#[test]
fn uint_literal_and_arithmetic() {
    let src = "fun box(): String {\n\
    val a: UInt = 5u\n\
    val b: UInt = 3u\n\
    if (a + b != 8u) return \"add ${a + b}\"\n\
    if (a * b != 15u) return \"mul ${a * b}\"\n\
    if (a - b != 2u) return \"sub ${a - b}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UIntArith");
    assert_eq!(out, "OK");
}

#[test]
fn ulong_literal_and_arithmetic() {
    let src = "fun box(): String {\n\
    val a: ULong = 10uL\n\
    val b: ULong = 4uL\n\
    if (a + b != 14uL) return \"add ${a + b}\"\n\
    if (a - b != 6uL) return \"sub ${a - b}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "ULongArith");
    assert_eq!(out, "OK");
}

#[test]
fn ubyte_and_ushort() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val b: UShort = 40000u\n\
    if (a.toInt() != 200) return \"ub ${a.toInt()}\"\n\
    if (b.toInt() != 40000) return \"us ${b.toInt()}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteShort");
    assert_eq!(out, "OK");
}

/// `UByte`/`UShort` have no arithmetic of their own — Kotlin defines each operator as `toInt()`
/// (zero-extend) followed by the `UInt` one, so the result is a `UInt`, never the narrow type.
#[test]
fn ubyte_and_ushort_arithmetic_promotes_to_uint() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val b: UByte = 100u\n\
    val s: UShort = 40000u\n\
    val t: UShort = 1000u\n\
    if (a + b != 300u) return \"add ${a + b}\"\n\
    if (a - b != 100u) return \"sub ${a - b}\"\n\
    if (a / b != 2u) return \"div ${a / b}\"\n\
    if (a % b != 0u) return \"rem ${a % b}\"\n\
    if (s + t != 41000u) return \"sadd ${s + t}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteArith");
    assert_eq!(out, "OK");
}

/// Ordering is UNSIGNED: `200u` as a `UByte` is the byte `-56`, so a signed compare would invert it.
#[test]
fn ubyte_and_ushort_comparison_is_unsigned() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val b: UByte = 100u\n\
    val s: UShort = 40000u\n\
    val t: UShort = 1000u\n\
    if (!(b < a)) return \"lt\"\n\
    if (b > a) return \"gt\"\n\
    if (!(a >= b)) return \"ge\"\n\
    if (!(t < s)) return \"slt\"\n\
    if (a == b) return \"eq\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteCmp");
    assert_eq!(out, "OK");
}

/// Widening out of the sign-extended `byte`/`short` zero-extends; narrowing to the signed primitive
/// is the raw reinterpret (`200u.toByte()` is `-56`).
#[test]
fn ubyte_and_ushort_conversions() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val s: UShort = 40000u\n\
    if (a.toLong() != 200L) return \"toLong ${a.toLong()}\"\n\
    if (a.toUInt() != 200u) return \"toUInt ${a.toUInt()}\"\n\
    if (a.toByte() != (-56).toByte()) return \"toByte ${a.toByte()}\"\n\
    if (s.toInt() != 40000) return \"toInt ${s.toInt()}\"\n\
    if (200.toUByte() != a) return \"toUByte\"\n\
    if (40000.toUShort() != s) return \"toUShort\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteConv");
    assert_eq!(out, "OK");
}

/// An unsigned literal takes its EXPECTED type, exactly as a signed integer literal does.
#[test]
fn unsigned_literal_takes_the_expected_type() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val s: UShort = 40000u\n\
    val i: UInt = 7u\n\
    val l: ULong = 7u\n\
    if (a.toInt() != 200) return \"ub\"\n\
    if (s.toInt() != 40000) return \"us\"\n\
    if (i != 7u) return \"ui\"\n\
    if (l != 7uL) return \"ul\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UExpected");
    assert_eq!(out, "OK");
}

/// Interpolation prints the UNSIGNED decimal, not the signed `byte`/`short` the value is stored in.
#[test]
fn ubyte_and_ushort_interpolate_unsigned() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val s: UShort = 40000u\n\
    if (\"$a\" != \"200\") return \"ub $a\"\n\
    if (\"$s\" != \"40000\") return \"us $s\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteStr");
    assert_eq!(out, "OK");
}

/// A zero-extended `UByte`/`UShort` is an `Int` — boxing it at an erased generic boundary must reach
/// `Integer.valueOf`. Typing the mask node from its narrow left operand picked `Byte.valueOf` (which
/// throws above 127) and `Short.valueOf` (which silently wraps to a negative).
#[test]
fn widened_ubyte_and_ushort_box_as_int() {
    let src = "fun box(): String {\n\
    val a: UByte = 200u\n\
    val s: UShort = 40000u\n\
    if (listOf(a.toInt()) != listOf(200)) return \"ub ${listOf(a.toInt())}\"\n\
    if (listOf(s.toInt()) != listOf(40000)) return \"us ${listOf(s.toInt())}\"\n\
    if (listOf((a + a).toInt()) != listOf(400)) return \"sum ${listOf((a + a).toInt())}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteBox");
    assert_eq!(out, "OK");
}

/// `inc`/`dec` stay INSIDE the narrow representation: `UByte` wraps at 255, `UShort` at 65535. Without
/// the `i2b`/`i2s` the sum leaves the byte/short and stops comparing equal to the same value written
/// as a literal — `==` is bit equality on that representation.
#[test]
fn ubyte_and_ushort_inc_dec_wrap_in_representation() {
    let src = "fun box(): String {\n\
    val a: UByte = 127u\n\
    if (a.inc() != 128u.toUByte()) return \"inc ${a.inc().toInt()}\"\n\
    val b: UByte = 255u\n\
    if (b.inc() != 0u.toUByte()) return \"wrap ${b.inc().toInt()}\"\n\
    val c: UByte = 0u\n\
    if (c.dec() != 255u.toUByte()) return \"dec ${c.dec().toInt()}\"\n\
    val s: UShort = 32767u\n\
    if (s.inc() != 32768u.toUShort()) return \"sinc ${s.inc().toInt()}\"\n\
    val t: UShort = 65535u\n\
    if (t.inc() != 0u.toUShort()) return \"swrap ${t.inc().toInt()}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UByteIncDec");
    assert_eq!(out, "OK");
}

#[test]
fn uint_comparison() {
    let src = "fun box(): String {\n\
    val a: UInt = 5u\n\
    val b: UInt = 9u\n\
    if (!(a < b)) return \"lt\"\n\
    if (a > b) return \"gt\"\n\
    if (!(b >= a)) return \"ge\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UIntCmp");
    assert_eq!(out, "OK");
}

#[test]
fn uint_conversions() {
    let src = "fun box(): String {\n\
    val i = 42\n\
    val u = i.toUInt()\n\
    if (u != 42u) return \"toUInt $u\"\n\
    if (u.toInt() != 42) return \"toInt ${u.toInt()}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UIntConv");
    assert_eq!(out, "OK");
}

#[test]
fn uint_overflow_wrap() {
    let src = "fun box(): String {\n\
    val max = UInt.MAX_VALUE\n\
    if (max + 1u != 0u) return \"wrap ${max + 1u}\"\n\
    if (0u - 1u != UInt.MAX_VALUE) return \"under ${0u - 1u}\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UIntWrap");
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_in_when() {
    let src = "fun classify(u: UInt): String = when (u) {\n\
    0u -> \"zero\"\n\
    1u -> \"one\"\n\
    else -> \"many\"\n\
}\n\
fun box(): String {\n\
    if (classify(0u) != \"zero\") return \"z\"\n\
    if (classify(1u) != \"one\") return \"o\"\n\
    if (classify(5u) != \"many\") return \"m\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UWhen");
    assert_eq!(out, "OK");
}

#[test]
fn uint_array() {
    let src = "fun box(): String {\n\
    val arr = UIntArray(3)\n\
    arr[0] = 10u\n\
    arr[1] = 20u\n\
    arr[2] = 30u\n\
    var sum = 0u\n\
    for (x in arr) sum += x\n\
    if (sum != 60u) return \"got $sum\"\n\
    return \"OK\"\n\
}\n";
    let out = run(src, "UIntArr");
    assert_eq!(out, "OK");
}
