//! `==` / `!=` between two nullable boxed numbers of possibly-different types (`Int? == Long?`): Kotlin
//! compares by VALUE with numeric promotion (and IEEE-754 for FP), NOT by `equals` (which is
//! type-specific — `Integer.equals(Long)` is always false). Lowered to a conform: null handling, then
//! promote both to the wider primitive and primitive-compare. Round-tripped on the JVM.
//!
//! (The smart-cast form `x is Double? && y is Int? && x == y` additionally needs chained-`&&` narrowing
//! in the checker — a separate piece — to reach this conform.)

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
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
