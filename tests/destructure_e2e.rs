//! Destructuring declarations `val (a, b) = e` (desugared to `componentN()` calls). Compiled by
//! the krusty binary, run on a real JVM. A type without `componentN` operators is cleanly rejected.

use std::fs;
use std::process::Command;

use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

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

#[test]
fn destructuring_runs_correctly() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping destructure_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_destr_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("Destr.kt");
    fs::write(&src_path, SRC).unwrap();
    let bin = env!("CARGO_BIN_EXE_krusty");
    let out = Command::new(bin).args(["-d", dir.to_str().unwrap()]).arg(&src_path).output().unwrap();
    assert!(out.status.success(), "krusty bin: {}", String::from_utf8_lossy(&out.stderr));
    let main = "public class M { public static void main(String[] a) { System.out.println(DestrKt.box()); } }";
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn destructuring_non_component_type_is_rejected() {
    // `String` has no `component1`/`component2` operators → must be rejected, never miscompiled.
    let msgs = check("fun box(): String {\n    val s = \"ab\"\n    val (a, b) = s\n    return \"OK\"\n}\n");
    assert!(msgs.iter().any(|m| m.contains("cannot destructure")), "msgs: {msgs:?}");
}
