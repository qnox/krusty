//! `try`/`catch`: the body is guarded by a `Code` exception-table range; each `catch` clause stores
//! the caught exception into its variable and produces the result value. `try` is an expression (its
//! value is the body's, or a matching catch's). Multiple catches dispatch in order (subtype first).
//! `try` is only sound where the operand stack is empty at entry — other positions are rejected.
//! Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::jvm::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

fn compile(src: &str, internal: &str) -> (Vec<u8>, Vec<String>) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures_with_cp(&files, common::stdlib_classpath(), &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    (bytes, d.diags.iter().map(|x| x.msg.clone()).collect())
}

const SRC: &str = r#"
fun parse(s: String): Int = try {
    if (s == "bad") throw NumberFormatException("nope")
    s.length
} catch (e: NumberFormatException) {
    -1
}

fun classify(x: Int): String {
    try {
        if (x < 0) throw IllegalArgumentException("neg")
        if (x == 0) throw IllegalStateException("zero")
        return "pos"
    } catch (e: IllegalArgumentException) {
        return "neg"
    } catch (e: RuntimeException) {
        return "other"
    }
}

fun box(): String {
    if (parse("hello") != 5) return "f1"
    if (parse("bad") != -1) return "f2"
    if (classify(3) != "pos") return "f3"
    if (classify(-1) != "neg") return "f4"
    if (classify(0) != "other") return "f5"
    return "OK"
}
"#;

#[test]
fn try_catch_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping try_catch_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "TcKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_tc_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("TcKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(TcKt.box()); } }",
    )
    .unwrap();
    let jc = Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("M.java"))
        .output()
        .unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn try_in_string_concat_position_compiles_correctly() {
    // `"" + try { "O" }` is now supported: krusty evaluates the try first (empty stack),
    // saves the result to a local, then does the concat — the try entry always sees an empty stack.
    let src = "fun box(): String = \"\" + try { \"O\" } catch (e: Exception) { \"1\" }";
    let (_b, errs) = compile(src, "TryConcat");
    assert!(errs.is_empty(), "unexpected compile errors: {errs:?}");
}

#[test]
fn try_finally_is_rejected() {
    let src = "fun box(): String { try { return \"a\" } catch (e: Exception) { return \"b\" } finally { } }";
    let (_b, errs) = compile(src, "FinKt");
    assert!(errs.iter().any(|m| m.contains("finally")), "expected finally rejection, got {errs:?}");
}
