//! `&&` / `||` must SHORT-CIRCUIT: the right operand is not evaluated when the left decides the result.
//! Regression guard for the eager-`iand`/`ior` miscompile (`x != 0 && 10/x > 0` must not divide when
//! `x == 0`). Compiled by the krusty binary, run on a real JVM under `-Xverify:all`.

mod common;

/// Compile `src` in-process and run `SCKt.box()` on the shared persistent JVM (stdlib + JDK jimage on
/// the classpath). `None` ⇒ environment unavailable (skip) or krusty couldn't emit.
fn run_box(_tag: &str, src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk_modules = common::jdk_modules()?;
    common::compile_and_run_box(src, "SC", &[stdlib], Some(&jdk_modules))
}

#[test]
fn and_short_circuits_throwing_rhs() {
    // `x != 0` is false, so `10 / x` must NOT be evaluated (eager `iand` would divide by zero).
    let src = "fun box(): String {\n  val x = 0\n  val ok = x != 0 && 10 / x > 0\n  return if (!ok) \"OK\" else \"FAIL\"\n}\n";
    if let Some(out) = run_box("and", src) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn or_short_circuits_throwing_rhs() {
    // `x == 0` is true, so `10 / x` must NOT be evaluated.
    let src = "fun box(): String {\n  val x = 0\n  val ok = x == 0 || 10 / x > 0\n  return if (ok) \"OK\" else \"FAIL\"\n}\n";
    if let Some(out) = run_box("or", src) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn const_bool_and_or_fold() {
    // Constant-foldable boolean operands (as in a `const val`) must still work.
    let src = "fun box(): String {\n  val a = false && (1 / 0 > 0)\n  val b = true || (1 / 0 > 0)\n  return if (!a && b) \"OK\" else \"FAIL\"\n}\n";
    if let Some(out) = run_box("const", src) {
        assert_eq!(out, "OK");
    }
}
