//! Raw string literals (`"""..."""`): content is verbatim — no escape processing, may span lines,
//! and may contain single/double quotes. Interpolation inside a raw string is rejected (skipped),
//! not mis-lexed. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Expr;
use krusty::codegen::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = "fun box(): String {\n    val s = \"\"\"a\nb\\nc \"q\" \"\"\"\n    if (s != \"a\\nb\\\\nc \\\"q\\\" \") return \"f1\"\n    return \"OK\"\n}\n";

#[test]
fn raw_string_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping raw_string_e2e: set JAVA_HOME");
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
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let dir = std::env::temp_dir().join(format!("krusty_raw_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("RawKt.class"), emit_file(&files[0], &info, &syms, "RawKt", &mut d).0).unwrap();
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(RawKt.box()); } }",
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
fn raw_string_value_is_verbatim() {
    // `\n` inside a raw string is two characters (backslash, n), not a newline.
    let src = "fun f(): String = \"\"\"x\\ny\"\"\"";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    assert!(!d.has_errors(), "parse errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    let lit = file.expr_arena.iter().find_map(|e| match e {
        Expr::StringLit(s) => Some(s.clone()),
        _ => None,
    });
    assert_eq!(lit.as_deref(), Some("x\\ny"));
}

#[test]
fn raw_string_interpolation_is_rejected() {
    let src = "fun f(): String { val x = 1\n return \"\"\"v=$x\"\"\" }";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let _ = parse(src, &toks, &mut d);
    assert!(d.has_errors(), "expected raw-string interpolation to be rejected");
}
