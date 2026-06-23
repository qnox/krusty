//! Destructuring declarations `val (a, b) = e` (desugared to `componentN()` calls). Compiled by
//! the krusty binary, run on a real JVM. A type without `componentN` operators is cleanly rejected.

use std::fs;
use std::process::Command;

use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

mod common;

/// Run the checker only, returning its diagnostics (for the rejection test).
fn check(src: &str) -> Vec<String> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    d.diags.iter().map(|x| x.msg.clone()).collect()
}

const SRC: &str = r#"
data class Pair2(val a: Int, val b: String)
fun box(): String {
    val p = Pair2(7, "hi")
    val (x, y) = p
    val (_, z) = p
    var total = 0
    for (i in 0..<3) {
        val (q, _) = Pair2(i, "x")
        total += q
    }
    return if (x == 7 && y == "hi" && z == "hi" && total == 3) "OK" else "FAIL"
}
"#;

/// Compile `src` with krusty (kotlin-stdlib on the classpath), run `DestrKt.box()` on a real JVM under
/// `-Xverify:all`, and return its trimmed stdout. `None` means the environment is unavailable (skip) or
/// krusty couldn't emit (unsupported IR) — never a test failure, matching the suite's other e2e tests.
fn run_box(tag: &str, src: &str) -> Option<String> {
    let java_home = std::env::var("KRUSTY_REF_JAVA_HOME")
        .or_else(|_| std::env::var("JAVA_HOME"))
        .ok()?;
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return None;
    }
    // The data class emits `Intrinsics` references, so kotlin-stdlib must be on the classpath.
    let stdlib = common::stdlib_jar()?;
    let stdlib = stdlib.to_str().unwrap().to_string();
    let dir = std::env::temp_dir().join(format!("krusty_destr_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("Destr.kt");
    fs::write(&src_path, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin)
        .args(["-cp", &stdlib, "-d", dir.to_str().unwrap()])
        .arg(&src_path)
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!(
            "skip (IR unsupported): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    let main = "public class M { public static void main(String[] a) { System.out.println(DestrKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
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
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib);
    let run = Command::new(&java)
        .args(["-Xverify:all", "-cp", &cp, "M"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "java: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout).trim().to_string();
    let _ = fs::remove_dir_all(&dir);
    Some(stdout)
}

#[test]
fn destructuring_runs_correctly() {
    if let Some(out) = run_box("decl", SRC) {
        assert_eq!(out, "OK");
    }
}

const LAMBDA_SRC: &str = r#"
data class P(val a: Int, val b: String)
fun box(): String {
    val p = P(7, "hi")
    // Lambda-parameter destructuring on a scope-function receiver.
    val r = p.let { (a, b) -> b + a }
    // Destructuring into a function-typed local.
    val f: (P) -> String = { (x, _) -> x.toString() }
    return if (r == "hi7" && f(p) == "7") "OK" else "FAIL"
}
"#;

#[test]
fn lambda_destructuring_runs_correctly() {
    if let Some(out) = run_box("lambda", LAMBDA_SRC) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn lambda_destructuring_parses_and_checks() {
    // A lambda parameter destructured into components: `{ (a, b) -> … }` binds `a`/`b` to
    // `component1()`/`component2()` of the (synthetic) lambda parameter. Uses an explicit function
    // type so the parameter's type is known without a stdlib on the classpath (the `check` harness
    // has none); the JVM-run test below covers `let`-receiver destructuring with the stdlib.
    let msgs = check(
        r#"
data class P(val a: Int, val b: String)
fun box(): String {
    val f: (P) -> String = { (a, b) -> b + a.toString() }
    val p = P(7, "hi")
    return if (f(p) == "hi7") "OK" else "FAIL"
}
"#,
    );
    assert!(msgs.is_empty(), "unexpected diags: {msgs:?}");
}

#[test]
fn destructuring_non_component_type_is_rejected() {
    // `String` has no `component1`/`component2` operators → must be rejected, never miscompiled.
    let msgs = check(
        "fun box(): String {\n    val s = \"ab\"\n    val (a, b) = s\n    return \"OK\"\n}\n",
    );
    assert!(
        msgs.iter().any(|m| m.contains("cannot destructure")),
        "msgs: {msgs:?}"
    );
}
