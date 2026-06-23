//! `&&` / `||` must SHORT-CIRCUIT: the right operand is not evaluated when the left decides the result.
//! Regression guard for the eager-`iand`/`ior` miscompile (`x != 0 && 10/x > 0` must not divide when
//! `x == 0`). Compiled by the krusty binary, run on a real JVM under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

/// Compile `src` with krusty (stdlib + JDK jimage on the classpath), run `SCKt.box()` on a JVM, return
/// trimmed stdout. `None` ⇒ environment unavailable (skip) or krusty couldn't emit.
fn run_box(tag: &str, src: &str) -> Option<String> {
    let java_home = std::env::var("KRUSTY_REF_JAVA_HOME")
        .or_else(|_| std::env::var("JAVA_HOME"))
        .ok()?;
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return None;
    }
    let stdlib = common::stdlib_jar()?;
    let stdlib = stdlib.to_str().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("krusty_sc_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SC.kt"), src).unwrap();
    let kcp = format!("{stdlib}:{java_home}/lib/modules");
    let out = Command::new(env!("CARGO_BIN_EXE_krusty"))
        .args(["-cp", &kcp, "-d", dir.to_str().unwrap()])
        .arg(dir.join("SC.kt"))
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!(
            "skip (unsupported): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a){ System.out.println(SCKt.box()); } }",
    )
    .unwrap();
    let jc = Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(
        jc.status.success(),
        "javac: {}",
        String::from_utf8_lossy(&jc.stderr)
    );
    let run = Command::new(format!("{java_home}/bin/java"))
        .args([
            "-Xverify:all",
            "-cp",
            &format!("{}:{stdlib}", dir.display()),
            "M",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "java: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let s = String::from_utf8_lossy(&run.stdout).trim().to_string();
    let _ = fs::remove_dir_all(&dir);
    Some(s)
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
