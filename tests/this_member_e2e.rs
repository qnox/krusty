//! Explicit `this` (a value of the enclosing class type, `aload 0`) and member assignment
//! (`receiver.prop = value`, incl. compound `+=`), written via the public setter so it works
//! cross-instance and dispatches correctly for open classes. Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
class Counter(var n: Int) {
    fun bump(): Counter {
        this.n = this.n + 1
        return this
    }
    fun value(): Int = this.n
}

class Acc(var sum: Int)

fun add(a: Acc, x: Int) {
    a.sum += x
}

fun box(): String {
    val c = Counter(10)
    c.bump().bump()
    if (c.value() != 12) return "f1"

    val a = Acc(0)
    add(a, 5)
    a.sum = a.sum + 2
    add(a, 3)
    if (a.sum != 10) return "f2"

    return "OK"
}
"#;

#[test]
fn this_and_member_assign_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping this_member_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_thismem_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ThisMemKt.class"), emit_file(&files[0], &info, &syms, "ThisMemKt", &mut d).0).unwrap();
    for name in ["Counter", "Acc"] {
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
        "public class M { public static void main(String[] a) { System.out.println(ThisMemKt.box()); } }",
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
fn assigning_a_val_member_is_rejected() {
    let src = "class P(val x: Int)\nfun f(p: P) { p.x = 5 }";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let _ = check_file(&files[0], &syms, &mut d);
    assert!(
        d.diags.iter().any(|x| x.msg.contains("val cannot be reassigned")),
        "expected val-reassignment rejection, got {:?}",
        d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
    );
}
