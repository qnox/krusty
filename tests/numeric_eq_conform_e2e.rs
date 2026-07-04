//! `==` / `!=` between two nullable boxed numbers of possibly-different types (`Int? == Long?`): Kotlin
//! compares by VALUE with numeric promotion (and IEEE-754 for FP), NOT by `equals` (which is
//! type-specific — `Integer.equals(Long)` is always false). Lowered to a conform: null handling, then
//! promote both to the wider primitive and primitive-compare. Round-tripped on the JVM.
//!
//! (The smart-cast form `x is Double? && y is Int? && x == y` additionally needs chained-`&&` narrowing
//! in the checker — a separate piece — to reach this conform.)

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn mixed_nullable_int_long() {
    const SRC: &str = "fun box(): String {\n\
    val a: Int? = 1\n\
    val b: Long? = 1L\n\
    val c: Long? = 2L\n\
    val n: Int? = null\n\
    val m: Long? = null\n\
    if (a != b) return \"f1\"\n\
    if (a == c) return \"f2\"\n\
    if (a == n) return \"f3\"\n\
    if (n != m) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Int? == Long? conforms"), "OK");
}

#[test]
fn mixed_nullable_int_double_ieee() {
    // FP promotion + IEEE: `1 == 1.0` true; `-0.0 == 0` true (IEEE); both null equal.
    const SRC: &str = "fun box(): String {\n\
    val i: Int? = 1\n\
    val d: Double? = 1.0\n\
    val z: Int? = 0\n\
    val mz: Double? = -0.0\n\
    if (i != d) return \"f1\"\n\
    if (z != mz) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Int?/Double? IEEE conform"), "OK");
}

#[test]
fn smartcast_chain_then_numeric_eq() {
    // The smart-cast form (the ieee754 cluster): `x is Double? && y is Int? && x == y` narrows BOTH x and
    // y across the `&&` chain, then conforms. IEEE: `-0.0 == 0` true; both-null equal; `0.0 == 1` false.
    const SRC: &str = "fun eqDI(x: Any?, y: Any?) = x is Double? && y is Int? && x == y\n\
fun box(): String {\n\
    if (!eqDI(null, null)) return \"f1\"\n\
    if (eqDI(null, 0)) return \"f2\"\n\
    if (!eqDI(-0.0, 0)) return \"f3\"\n\
    if (eqDI(0.0, 1)) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("smart-cast chain numeric == conforms"),
        "OK"
    );
}

#[test]
fn nullable_double_equals_primitive_ieee() {
    // A nullable-FP wrapper compared with its primitive (`Double? == Double`, `Double? == Double?`):
    // null-check then UNBOX + primitive `==` (`dcmp`, IEEE — `0.0 == 0.0`, null handling). Was wrongly
    // rejected as "operator cannot be applied to Double and Double".
    const SRC: &str = "fun eq(a: Double?, b: Double?) = a == b\n\
fun eqP(a: Double?, b: Double) = a == b\n\
fun box(): String {\n\
    if (!eq(null, null)) return \"f1\"\n\
    if (eq(null, 0.0)) return \"f2\"\n\
    if (!eq(0.0, 0.0)) return \"f3\"\n\
    if (eqP(null, 0.0)) return \"f4\"\n\
    if (!eqP(0.0, 0.0)) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("nullable Double == compiles + runs"), "OK");
}
