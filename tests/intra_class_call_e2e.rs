//! Unqualified calls to a sibling instance method (`foo()` inside a method → `this.foo()`,
//! `invokevirtual`), including up the base-class chain. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
open class Base {
    open fun base(): Int = 10
}
class C(val n: Int) : Base() {
    fun helper(): Int = n * 2
    fun result(): Int = helper() + n + base()
}
fun box(): String {
    val c = C(5)
    if (c.result() != 25) return "f1"
    return "OK"
}
"#;

#[test]
fn intra_class_call_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping intra_class_call_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_ic_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("IcKt.class"), emit_file(&files[0], &info, &syms, "IcKt", &mut d).0).unwrap();
    for name in ["Base", "C"] {
        let cd = files[0]
            .decls
            .iter()
            .find_map(|&id| match files[0].decl(id) {
                Decl::Class(c) if c.name == name => Some(c.clone()),
                _ => None,
            })
            .expect("decl");
        fs::write(dir.join(format!("{name}.class")), emit_class(&cd, &files[0], &info, name, name, &syms, &mut d).0).unwrap();
    }
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    fs::write(
        dir.join("M.java"),
        "public class M { public static void main(String[] a) { System.out.println(IcKt.box()); } }",
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
