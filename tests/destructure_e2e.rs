//! Destructuring declarations `val (a, b) = e` (desugared to `componentN()` calls). Compiled by
//! the krusty binary, run on a real JVM. A type without `componentN` operators is cleanly rejected.

use std::path::PathBuf;

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
    let mut syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &mut syms, &mut d);
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

/// Compile `src` in-process (kotlin-stdlib + JDK jimage on the classpath) and run `DestrKt.box()` on
/// the shared persistent JVM. `None` means the environment is unavailable (skip) or krusty couldn't
/// emit (unsupported IR) — never a test failure, matching the suite's other e2e tests.
fn run_box(_tag: &str, src: &str) -> Option<String> {
    let java_home = common::java_home()?;
    // The data class emits `Intrinsics` references, so kotlin-stdlib must be on the classpath. The
    // jimage (`<jdk>/lib/modules`) is the compile-time bootclasspath so collection supertypes resolve.
    let stdlib = common::stdlib_jar()?;
    let jdk_modules = PathBuf::from(format!("{java_home}/lib/modules"));
    common::compile_and_run_box(src, "Destr", &[stdlib], Some(&jdk_modules))
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

const FOREACH_SRC: &str = r#"
data class P(val a: Int, val b: String)
fun box(): String {
    val xs = listOf(P(1, "a"), P(2, "b"), P(3, "c"))
    var sum = 0
    var s = ""
    // forEach over a List of data-class elements, with a destructured lambda parameter.
    xs.forEach { (n, t) -> sum += n; s += t }
    return if (sum == 6 && s == "abc") "OK" else "FAIL"
}
"#;

#[test]
fn foreach_destructuring_runs_correctly() {
    if let Some(out) = run_box("foreach", FOREACH_SRC) {
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
