//! End-to-end "box" coverage for value/inline classes and unsigned types. Each test compiles a
//! `fun box(): String` with krusty, runs it on a real JVM under verification, and asserts `"OK"`.
//! Targets `src/jvm/value_classes.rs` and `src/jvm/inline.rs`.

use super::common;

fn run(src: &str, stem: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, stem)
}

#[test]
fn value_class_construct_read() {
    let src = "@JvmInline value class X(val v: Int)\n\
fun box(): String {\n\
    val x = X(7)\n\
    if (x.v != 7) return \"got ${x.v}\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(src, "VcRead") else {
        return;
    };
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
    let Some(out) = run(src, "VcAdd") else { return };
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
    let Some(out) = run(src, "VcRet") else { return };
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
    let Some(out) = run(src, "VcStr") else { return };
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
    let Some(out) = run(src, "VcMember") else {
        return;
    };
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
    let Some(out) = run(src, "VcIface") else {
        return;
    };
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
    let Some(out) = run(src, "VcGeneric") else {
        return;
    };
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
    let Some(out) = run(src, "VcNullable") else {
        return;
    };
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
    let Some(out) = run(src, "VcMapKey") else {
        return;
    };
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
    let Some(out) = run(src, "UIntArith") else {
        return;
    };
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
    let Some(out) = run(src, "ULongArith") else {
        return;
    };
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
    let Some(out) = run(src, "UByteShort") else {
        return;
    };
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
    let Some(out) = run(src, "UIntCmp") else {
        return;
    };
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
    let Some(out) = run(src, "UIntConv") else {
        return;
    };
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
    let Some(out) = run(src, "UIntWrap") else {
        return;
    };
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
    let Some(out) = run(src, "UWhen") else { return };
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
    let Some(out) = run(src, "UIntArr") else {
        return;
    };
    assert_eq!(out, "OK");
}
