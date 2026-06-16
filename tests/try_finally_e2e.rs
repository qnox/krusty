//! `try`/`finally`: the finally block runs on every exit — normal completion of the body, after a
//! catch, and (via a catch-all that re-throws) when an exception propagates. krusty supports the
//! "pure cleanup" case (no `return`/`break`/`continue` escaping the guarded region, Unit/Nothing
//! body); other cases are rejected. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

const SRC: &str = r#"
class Log { var s: String = "" }

fun run1(log: Log, fail: Boolean) {
    try {
        log.s += "a"
        if (fail) throw RuntimeException("x")
        log.s += "b"
    } catch (e: RuntimeException) {
        log.s += "c"
    } finally {
        log.s += "f"
    }
}

fun risky(log: Log) {
    try {
        log.s += "t"
        throw IllegalStateException("boom")
    } finally {
        log.s += "F"
    }
}

fun box(): String {
    val l1 = Log(); run1(l1, false)
    if (l1.s != "abf") return "f1"
    val l2 = Log(); run1(l2, true)
    if (l2.s != "acf") return "f2"
    val l3 = Log()
    try { risky(l3) } catch (e: IllegalStateException) { l3.s += "C" }
    if (l3.s != "tFC") return "f3"
    return "OK"
}
"#;

#[test]
fn try_finally_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping try_finally_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let file = parse(SRC, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures_with_cp(&files, common::stdlib_classpath(), &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let dir = std::env::temp_dir().join(format!("krusty_tf_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("TfKt.class"), emit_file(&files[0], &info, &syms, "TfKt", &mut d).0).unwrap();
    let log = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == "Log" => Some(c.clone()),
            _ => None,
        })
        .expect("Log decl");
    fs::write(dir.join("Log.class"), emit_class(&log, &files[0], &info, "Log", "Log", &syms, &mut d).0).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(TfKt.box()); } }",
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
