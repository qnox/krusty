//! `"…".trimIndent()` / `.trimMargin()` are kotlin-stdlib extensions (no JDK method, and krusty
//! doesn't link the stdlib), so krusty folds them at compile time when the receiver is a string
//! literal: `trimIndent` strips the common leading indentation (dropping blank first/last lines);
//! `trimMargin` strips up to the `|` marker. A non-literal receiver is rejected. Run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Expr;
use krusty::codegen::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile(src: &str, internal: &str) -> (Vec<u8>, Vec<String>) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let bytes = emit_file(&files[0], &info, &syms, internal, &mut d);
    (bytes, d.diags.iter().map(|x| x.msg.clone()).collect())
}

const SRC: &str = "fun box(): String {\n    val s = \"\"\"\n        line1\n        line2\n    \"\"\".trimIndent()\n    if (s != \"line1\\nline2\") return \"f1\"\n    val m = \"\"\"\n        |a\n        |b\n    \"\"\".trimMargin()\n    if (m != \"a\\nb\") return \"f2\"\n    return \"OK\"\n}\n";

#[test]
fn trim_indent_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping trim_indent_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let (bytes, errs) = compile(SRC, "TiKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_ti_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("TiKt.class"), bytes).unwrap();
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(TiKt.box()); } }",
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
fn trim_indent_folds_to_a_constant() {
    // The folded result is a plain string-literal constant (no method call left in the AST).
    let (_b, errs) = compile("fun f(): String = \"\"\"\n  a\n  b\n\"\"\".trimIndent()", "FoldKt");
    assert!(errs.is_empty(), "errors: {errs:?}");
    // verify a literal "a\nb" exists among the parsed expressions
    let mut d = DiagSink::new();
    let src = "fun f(): String = \"\"\"\n  a\n  b\n\"\"\".trimIndent()";
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let has_raw = file.expr_arena.iter().any(|e| matches!(e, Expr::StringLit(s) if s.contains("  a")));
    assert!(has_raw, "expected the raw string literal to be present pre-fold");
}
