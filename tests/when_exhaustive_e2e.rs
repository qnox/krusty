//! Exhaustive `when` without `else` over a `sealed` hierarchy: when the subject is a sealed type and
//! every subclass is matched by an `is` arm, the `when` is an expression (its value is the join of
//! the arm bodies). The unreachable no-match path throws (mirroring Kotlin's
//! `NoWhenBranchMatchedException`) so the JVM verifier sees every path produce a value or diverge.
//! A non-exhaustive `when` used as an expression is rejected (skipped).

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
sealed class Expr
class Num(val v: Int) : Expr()
class Add(val a: Int, val b: Int) : Expr()

fun eval(e: Expr): Int = when (e) {
    is Num -> e.v
    is Add -> e.a + e.b
}

fun box(): String {
    if (eval(Num(7)) != 7) return "f1"
    if (eval(Add(3, 4)) != 7) return "f2"
    return "OK"
}
"#;

#[test]
fn exhaustive_sealed_when_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping when_exhaustive_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_we_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("WeKt.class"), emit_file(&files[0], &info, &syms, "WeKt", &mut d).0).unwrap();
    for name in ["Expr", "Num", "Add"] {
        let cd = files[0]
            .decls
            .iter()
            .find_map(|&id| match files[0].decl(id) {
                Decl::Class(c) if c.name == name => Some(c.clone()),
                _ => None,
            })
            .expect("class decl");
        fs::write(dir.join(format!("{name}.class")), emit_class(&cd, &files[0], &info, name, name, &syms, &mut d).0).unwrap();
    }
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(WeKt.box()); } }",
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
fn non_exhaustive_when_is_rejected() {
    let src = r#"
sealed class Expr
class Num(val v: Int) : Expr()
class Add(val a: Int, val b: Int) : Expr()
fun eval(e: Expr): Int = when (e) {
    is Num -> e.v
}
"#;
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(d.has_errors(), "expected a non-exhaustive when (no else) to be rejected");
}
