//! `throw` expressions (bottom type `Nothing`) and construction of common JDK exceptions by simple
//! name (`RuntimeException("msg")`, `IllegalStateException()`, …). `throw` works as a statement, a
//! function body, and inside `?:` / `if` guards. Compiled by krusty and run on a real JVM.

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
fun req(x: Int): Int {
    if (x < 0) throw RuntimeException("neg")
    return x
}
fun pick(s: String?): String = s ?: throw IllegalStateException("was null")
fun box(): String {
    if (req(5) != 5) return "f1"
    if (pick("hi") != "hi") return "f2"
    return "OK"
}
"#;

#[test]
fn throw_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping throw_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    let (bytes, errs) = compile(SRC, "ThrowKt");
    assert!(errs.is_empty(), "krusty errors: {errs:?}");
    let dir = std::env::temp_dir().join(format!("krusty_throw_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ThrowKt.class"), bytes).unwrap();
    // The guard path also actually throws and preserves the message.
    fs::write(
        dir.join("M.java"),
        r#"public class M { public static void main(String[] a) {
            if (!ThrowKt.box().equals("OK")) { System.out.println("box-fail"); return; }
            try { ThrowKt.req(-1); System.out.println("no-throw"); }
            catch (RuntimeException e) { System.out.println(e.getMessage()); }
        } }"#,
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
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "neg");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn inline_class_is_rejected() {
    // `inline`/`value class` has unboxed semantics krusty doesn't model — it must be rejected, not
    // silently compiled as a normal class (which would miscompile `==`).
    let (_b, errs) = compile("inline class Z(val x: Int)\nfun box(): String = \"OK\"", "ZKt");
    assert!(errs.iter().any(|m| m.contains("value/inline")), "expected value/inline rejection, got {errs:?}");
}
