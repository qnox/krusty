//! Subject-form `when` with type-test arms: `when (x) { is T -> … }` dispatches via `instanceof`
//! against the subject (evaluated once into a slot), and `x` is smart-cast to `T` inside the arm.
//! Compiled by krusty and run on a real JVM.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::{emit_class, emit_file};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
open class Shape
class Circle(val r: Int) : Shape()
class Square(val s: Int) : Shape()

fun area(sh: Shape): String = when (sh) {
    is Circle -> "circle" + sh.r
    is Square -> "square" + sh.s
    else -> "unknown"
}

fun box(): String {
    if (area(Circle(3)) != "circle3") return "f1"
    if (area(Square(4)) != "square4") return "f2"
    if (area(Shape()) != "unknown") return "f3"
    return "OK"
}
"#;

#[test]
fn when_is_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping when_is_e2e: set JAVA_HOME");
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

    let dir = std::env::temp_dir().join(format!("krusty_wis_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("WisKt.class"), emit_file(&files[0], &info, &syms, "WisKt", &mut d).0).unwrap();
    for name in ["Shape", "Circle", "Square"] {
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
        "public class M { public static void main(String[] a) { System.out.println(WisKt.box()); } }",
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
