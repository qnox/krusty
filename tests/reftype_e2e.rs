//! Reference types (`Ty::Obj`): krusty classes as values — construction (`Point(1,2)`), class-typed
//! parameters/returns, property access (`p.x`), and instance-method dispatch between Kotlin classes.
//! Compiled by krusty, then loaded/verified/run on a real JVM via a Java `Main`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::jvm::emit::{emit_class, emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};
use krusty::ast::Decl;

/// Compile a single source file (classes + a `RefKt` facade) to a dir of `.class` files.
fn compile_to(dir: &PathBuf, src: &str) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    for &id in &files[0].decls {
        if let Decl::Class(c) = files[0].decl(id) {
            let (bytes, _) = emit_class(c, &files[0], &info, &c.name, &c.name, &syms, &mut d);
            fs::write(dir.join(format!("{}.class", c.name)), bytes).unwrap();
        }
    }
    let has_funs = files[0].decls.iter().any(|&id| matches!(files[0].decl(id), Decl::Fun(_)));
    if has_funs {
        let internal = file_class_name("Ref", None);
        let (bytes, _) = emit_file(&files[0], &info, &syms, &internal, &mut d);
        fs::write(dir.join(format!("{internal}.class")), bytes).unwrap();
    }
    assert!(!d.has_errors(), "krusty codegen errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
}

#[test]
fn reference_types_construct_access_and_dispatch() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping reftype_e2e: set JAVA_HOME or KRUSTY_REF_JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        eprintln!("skipping: no javac at {javac}");
        return;
    }

    // A class with a class-typed property + a member function taking/returning a class type, plus
    // top-level functions that construct a class, read its property, and call its method.
    let src = r#"
class Point(val x: Int, val y: Int) {
  fun sum(): Int = x + y
  fun translated(d: Int): Point = Point(x + d, y + d)
}
class Line(val from: Point, val to: Point) {
  fun span(): Int = to.x - from.x
}
fun makeLine(a: Int, b: Int): Line = Line(Point(a, a), Point(b, b))
fun probe(): Int {
  val l = makeLine(2, 5)
  val moved = l.to.translated(10)
  return l.span() + l.from.sum() + moved.x
}
"#;

    let dir = std::env::temp_dir().join(format!("krusty_ref_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    compile_to(&dir, src);

    // span = 5-2 = 3 ; from.sum = 2+2 = 4 ; moved = (5+10) -> x=15 ; total = 3+4+15 = 22
    let main = r#"
public class Main {
    public static void main(String[] a) {
        System.out.println(RefKt.probe());
    }
}"#;
    fs::write(dir.join("Main.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("Main.java")).output().expect("javac");
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"]).output().expect("java");
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "22\n");

    let _ = fs::remove_dir_all(&dir);
}
