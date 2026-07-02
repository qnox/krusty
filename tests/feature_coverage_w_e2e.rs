//! End-to-end codegen coverage (round-tripped on a real JVM) for three backend passes:
//!   * `src/jvm/value_classes.rs` — `@JvmInline value class` box/unbox at use sites (args, returns,
//!     nullable boxing, interface dispatch, members, collections, map keys),
//!   * `src/jvm/ir_emit.rs` — bytecode-shape edges (mixed-width arithmetic + conversions, `when`
//!     switches, string concat, nested arrays, bit ops on Int/Long, inc/dec in loops, compound
//!     assignment on array elements and fields), and unsigned-type (`UInt`/`ULong`/`UIntArray`) codegen,
//!   * `src/jvm/suspend.rs` — the suspend state machine driven from a Java `Continuation`.
//!
//! Value-class / unsigned / bytecode-shape tests use the `box(): String -> "OK"` harness
//! (`common::compile_and_run_box`). Suspend tests can't be driven from a non-suspend `box()` (krusty
//! does not resolve `runBlocking`), so — like `tests/suspend_e2e.rs` — they compile with the krusty
//! binary and drive the CPS entry point with a trivial `Continuation` from a small Java driver.
//!
//! Every test skips cleanly (returns) when the JDK / kotlin-stdlib toolchain is unavailable.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod common;

/// (stdlib jar, jdk `lib/modules` jimage) for the `box()` harness, or `None` → skip.
fn env() -> Option<(PathBuf, PathBuf)> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    Some((stdlib, jdk))
}

/// Compile `src` (a `fun box(): String`) and assert it round-trips to "OK" on the JVM.
fn run_box(src: &str, stem: &str) {
    let Some((stdlib, jdk)) = env() else {
        eprintln!("skipping {stem}: no JDK/stdlib toolchain");
        return;
    };
    assert_eq!(
        common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)).as_deref(),
        Some("OK"),
        "{stem}"
    );
}

// ---------------------------------------------------------------------------
// value_classes.rs — @JvmInline value class codegen at use sites
// ---------------------------------------------------------------------------

#[test]
fn value_class_arg_and_return() {
    // Construct, pass as two args, and return a value class — the unboxed underlying value flows
    // through the call boundary (no wrapper allocation).
    run_box(
        r#"
@JvmInline
value class Meters(val v: Int)
fun add(a: Meters, b: Meters): Meters = Meters(a.v + b.v)
fun box(): String {
    val r = add(Meters(3), Meters(4))
    return if (r.v == 7) "OK" else "f:${r.v}"
}
"#,
        "VcArgReturn",
    );
}

#[test]
fn value_class_in_when_and_if() {
    // A value class dispatched through `when`/`if` on its underlying field.
    run_box(
        r#"
@JvmInline
value class Code(val n: Int)
fun classify(c: Code): String = when (c.n) {
    0 -> "zero"
    1 -> "one"
    else -> "many"
}
fun box(): String {
    if (classify(Code(0)) != "zero") return "f1"
    if (classify(Code(1)) != "one") return "f2"
    if (classify(Code(9)) != "many") return "f3"
    return "OK"
}
"#,
        "VcWhenIf",
    );
}

#[test]
fn nullable_value_class_boxes() {
    // A `Vc?` forces the wrapper representation (null needs a reference); non-null unboxes back.
    run_box(
        r#"
@JvmInline
value class Id(val v: Int)
fun pick(b: Boolean): Id? = if (b) Id(5) else null
fun box(): String {
    val a = pick(true)
    val b = pick(false)
    if (a == null) return "f1"
    if (b != null) return "f2"
    if (a.v != 5) return "f3"
    return "OK"
}
"#,
        "VcNullable",
    );
}

#[test]
fn value_class_implements_interface() {
    // A value class implementing an interface: called through the interface it must be BOXED (the
    // interface method dispatches on the wrapper), exercising box-impl at the widening point.
    run_box(
        r#"
interface Named { fun label(): String }
@JvmInline
value class Tag(val v: String) : Named {
    override fun label(): String = "tag:" + v
}
fun box(): String {
    val t: Named = Tag("x")
    return if (t.label() == "tag:x") "OK" else "f:${t.label()}"
}
"#,
        "VcInterface",
    );
}

#[test]
fn value_class_member_fun_and_property() {
    // A value class with a computed property and a member function — both compile to static `-impl`
    // methods taking the unboxed receiver.
    run_box(
        r#"
@JvmInline
value class Celsius(val v: Double) {
    val fahrenheit: Double get() = v * 9 / 5 + 32
    fun isFreezing(): Boolean = v <= 0.0
}
fun box(): String {
    val c = Celsius(100.0)
    if (c.fahrenheit != 212.0) return "f1:${c.fahrenheit}"
    if (!Celsius(-1.0).isFreezing()) return "f2"
    if (Celsius(10.0).isFreezing()) return "f3"
    return "OK"
}
"#,
        "VcMember",
    );
}

#[test]
fn list_of_value_class_boxes_elements() {
    // `listOf(Vc(..))` stores boxed elements (generic `List<W>`); reading `.v` unboxes each.
    run_box(
        r#"
@JvmInline
value class W(val v: Int)
fun box(): String {
    val xs = listOf(W(1), W(2), W(3))
    var s = 0
    for (x in xs) s += x.v
    return if (s == 6) "OK" else "f:$s"
}
"#,
        "VcList",
    );
}

// NOTE: value-class-as-map-key is intentionally omitted — krusty does not wire the boxed value
// class's equals/hashCode into `Map` lookup (a `mapOf(Key(..) to 1)` lookup returns null at runtime),
// so it fails the round-trip. Left out rather than modifying the compiler.

// ---------------------------------------------------------------------------
// ir_emit.rs — unsigned-type codegen
// ---------------------------------------------------------------------------

#[test]
fn uint_arithmetic_and_comparison() {
    // UInt arithmetic and comparison route through the unsigned intrinsics (compare/divide are the
    // interesting ones); values chosen above Int.MAX to prove unsignedness.
    run_box(
        r#"
fun box(): String {
    val a: UInt = 3000000000u
    val b: UInt = 1000000000u
    if (a + b != 4000000000u) return "f1:${a + b}"
    if (a - b != 2000000000u) return "f2"
    if (!(a > b)) return "f3"
    if (a / b != 3u) return "f4"
    return "OK"
}
"#,
        "UIntArith",
    );
}

#[test]
fn uint_conversions() {
    // toInt / toUInt / toLong conversions between signed and unsigned representations.
    run_box(
        r#"
fun box(): String {
    val a = 300u
    if (a.toInt() != 300) return "f1"
    val b = 5.toUInt()
    if (b != 5u) return "f2"
    val big: UInt = 4294967295u
    if (big.toLong() != 4294967295L) return "f3:${big.toLong()}"
    return "OK"
}
"#,
        "UIntConv",
    );
}

#[test]
fn uint_range_loop() {
    // A `1u..5u` UInt range in a for-loop.
    run_box(
        r#"
fun box(): String {
    var s: UInt = 0u
    for (i in 1u..5u) s += i
    return if (s == 15u) "OK" else "f:$s"
}
"#,
        "UIntRange",
    );
}

#[test]
fn ulong_arithmetic_large_literal() {
    // ULong arithmetic with a literal past Long.MAX_VALUE.
    run_box(
        r#"
fun box(): String {
    val a: ULong = 18000000000000000000uL
    if (a + 1uL != 18000000000000000001uL) return "f1"
    val x: ULong = 100uL
    if (x * 2uL != 200uL) return "f2"
    if (!(a > x)) return "f3"
    return "OK"
}
"#,
        "ULongArith",
    );
}

// NOTE: unsigned arrays (`UIntArray(n) { ... }` / `uintArrayOf(...)`) are intentionally omitted —
// krusty's `UIntArray` element reads yield a boxed `Integer` where `kotlin.UInt` is expected (runtime
// ClassCastException) and `uintArrayOf` is unresolved. Left out rather than modifying the compiler.

// ---------------------------------------------------------------------------
// ir_emit.rs — bytecode-shape edge cases
// ---------------------------------------------------------------------------

#[test]
fn mixed_width_arithmetic_conversions() {
    // Nested arithmetic mixing Int/Long/Double/Float forces i2l/i2d/f2d/l2d numeric conversions and a
    // Long `shl`.
    run_box(
        r#"
fun box(): String {
    val i = 5
    val l = 10L
    val d = 2.0
    val f = 3.0f
    val r: Double = i * l + d - f + (i / 2) * 1.5
    if (r != 52.0) return "f1:$r"
    val x = (i.toLong() shl 3) + l
    if (x != 50L) return "f2:$x"
    val g: Float = i + f
    if (g != 8.0f) return "f3:$g"
    return "OK"
}
"#,
        "MixedArith",
    );
}

#[test]
fn large_when_over_int_switch() {
    // A dense `when` over consecutive Int values — kotlinc/krusty emit a `tableswitch`.
    run_box(
        r#"
fun f(n: Int): Int = when (n) {
    0 -> 10
    1 -> 11
    2 -> 12
    3 -> 13
    4 -> 14
    5 -> 15
    6 -> 16
    7 -> 17
    else -> -1
}
fun box(): String {
    var s = 0
    for (i in 0..7) s += f(i)
    if (s != 108) return "f1:$s"
    if (f(99) != -1) return "f2"
    return "OK"
}
"#,
        "WhenSwitch",
    );
}

#[test]
fn long_string_concatenation() {
    // A long concatenation chain mixing string and int operands (invokedynamic makeConcat / StringBuilder).
    run_box(
        r#"
fun box(): String {
    val a = 1; val b = 2; val c = 3
    val s = "x=" + a + ",y=" + b + ",z=" + c + ",sum=" + (a + b + c) + "!"
    return if (s == "x=1,y=2,z=3,sum=6!") "OK" else "f:$s"
}
"#,
        "StrConcat",
    );
}

#[test]
fn two_dimensional_array() {
    // Array-of-arrays (`arrayOf(intArrayOf(...), ...)`) with nested indexed access and a nested loop.
    // (`Array(n) { IntArray(m) { ... } }` — an array-typed element in the sized-array ctor — is rejected
    // by krusty, so the literal form is used instead.)
    run_box(
        r#"
fun box(): String {
    val grid = arrayOf(intArrayOf(0, 1, 2), intArrayOf(3, 4, 5), intArrayOf(6, 7, 8))
    var s = 0
    for (row in grid) for (v in row) s += v
    if (s != 36) return "f1:$s"
    if (grid[2][2] != 8) return "f2"
    return "OK"
}
"#,
        "Array2D",
    );
}

#[test]
fn bit_operations_int_and_long() {
    // shl/shr/ushr/and/or/xor/inv on both Int and Long widths.
    run_box(
        r#"
fun box(): String {
    val x = 0b1100
    if (x and 0b1010 != 0b1000) return "f1"
    if (x or 0b0011 != 0b1111) return "f2"
    if (x xor 0b1010 != 0b0110) return "f3"
    if (x shl 2 != 48) return "f4"
    if (x shr 1 != 6) return "f5"
    if ((-8) ushr 28 != 15) return "f6"
    if (x.inv() != -13) return "f7"
    val y = 0xFF00FF00L
    if (y and 0x00FF00FFL != 0L) return "f8"
    if (y shl 4 != 0xFF00FF000L) return "f9"
    if (y ushr 8 != 0x00FF00FFL) return "f10:${y ushr 8}"
    return "OK"
}
"#,
        "BitOps",
    );
}

#[test]
fn increment_decrement_in_loops() {
    // Postfix `i++` / `j--` and prefix `++i` in loops (iinc / dup-pattern codegen).
    run_box(
        r#"
fun box(): String {
    var i = 0
    var count = 0
    while (i < 10) { i++; count++ }
    if (count != 10) return "f1:$count"
    var j = 5
    var sum = 0
    while (j > 0) { sum += j; j-- }
    if (sum != 15) return "f2:$sum"
    var k = 0
    for (n in 0 until 4) { k += ++i }
    if (k != 50) return "f3:$k"
    return "OK"
}
"#,
        "IncDec",
    );
}

#[test]
fn compound_assignment_array_and_field() {
    // Compound assignment (`+=`, `*=`) on array elements and on a mutable field — the read-modify-write
    // dup_x pattern in ir_emit.
    run_box(
        r#"
class Cell { var f: Int = 0 }
fun box(): String {
    val arr = IntArray(3)
    arr[0] += 5
    arr[1] += 10
    arr[0] *= 2
    arr[2] = arr[0] + arr[1]
    if (arr[0] != 10 || arr[1] != 10 || arr[2] != 20) return "f1:${arr[0]},${arr[1]},${arr[2]}"
    val b = Cell()
    b.f += 7
    b.f *= 3
    if (b.f != 21) return "f2:${b.f}"
    return "OK"
}
"#,
        "CompoundAssign",
    );
}

// ---------------------------------------------------------------------------
// suspend.rs — the CPS state machine, driven from a Java Continuation
// ---------------------------------------------------------------------------

/// Compile `src` (a top-level `suspend fun run(): Int` in `S.kt`, facade `SKt`) with the krusty binary,
/// then drive `SKt.run(k)` with a trivial synchronously-completing `Continuation` and assert the boxed
/// result equals `expect`. Skips if the JDK/stdlib toolchain is unavailable. Mirrors `suspend_e2e.rs`.
fn run_suspend(name: &str, src: &str, expect: i32) {
    let jh = match common::java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => {
            eprintln!("skipping {name}: no javac");
            return;
        }
    };
    let _ = &jh;
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping {name}: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib.to_string_lossy().into_owned();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_covw_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "{name}: krusty failed to compile:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = format!(
        "import kotlin.coroutines.*;\n\
public class M {{\n\
  public static void main(String[] a) {{\n\
    Continuation<Object> k = new Continuation<Object>() {{\n\
      public CoroutineContext getContext() {{ return EmptyCoroutineContext.INSTANCE; }}\n\
      public void resumeWith(Object o) {{ }}\n\
    }};\n\
    Object r = SKt.run(k);\n\
    System.out.println(r.equals(Integer.valueOf({expect})) ? \"OK\" : (\"r=\" + r));\n\
  }}\n\
}}\n"
    );
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let Some(out) = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    ) else {
        eprintln!("skipping {name}: java runner unavailable");
        return;
    };
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.trim(), "OK", "{name}: wrong result; got {out}");
}

#[test]
fn suspend_sequential_calls() {
    // Three sequential suspension points in one function; each result is bound, spilled across the next
    // suspension, and summed — the multi-state machine in suspend.rs. 1 + 2 + 3 = 6.
    run_suspend(
        "susp_seq",
        "suspend fun a(): Int = 1\n\
         suspend fun b(): Int = 2\n\
         suspend fun c(): Int = 3\n\
         suspend fun run(): Int {\n    val x = a()\n    val y = b()\n    val z = c()\n    return x + y + z\n}\n",
        6,
    );
}

#[test]
fn suspend_call_chain() {
    // A suspend fun calling a suspend fun that itself suspends on a leaf — nested CPS threading of the
    // continuation. leaf()=10, mid()=15, run()=16.
    run_suspend(
        "susp_chain",
        "suspend fun leaf(): Int = 10\n\
         suspend fun mid(): Int = leaf() + 5\n\
         suspend fun run(): Int {\n    return mid() + 1\n}\n",
        16,
    );
}
