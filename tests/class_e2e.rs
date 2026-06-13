//! Phase: classes with primary-constructor properties. krusty lowers `class C(val a, var b)` to a
//! JVM class with private backing fields, a primary constructor, and `getX`/`setX` accessors.
//! These tests (1) verify the emitted shape via the `.class` reader, and (2) load + verify + run
//! the class on a real JVM, calling its constructor and getters from Java.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::emit_class;
use krusty::diag::DiagSink;
use krusty::jvm::classreader::parse_class;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn compile_class(src: &str, class_name: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    assert!(!d.has_errors(), "parse errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "resolve errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    let cd = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == class_name => Some(c.clone()),
            _ => None,
        })
        .expect("class decl");
    emit_class(&cd, &files[0], &info, internal, &syms, &mut d)
}

#[test]
fn class_shape_matches_expected_abi() {
    let bytes = compile_class("class Point(val x: Int, var y: String)", "Point", "Point");
    let ci = parse_class(&bytes).expect("parse emitted class");

    assert_eq!(ci.this_class, "Point");
    assert_eq!(ci.super_class.as_deref(), Some("java/lang/Object"));

    // Backing fields: x is final, y is not (var).
    let x = ci.fields.iter().find(|f| f.name == "x").expect("field x");
    assert_eq!(x.descriptor, "I");
    assert!(x.access & krusty::codegen::classfile::ACC_FINAL != 0, "val backing field must be final");
    let y = ci.fields.iter().find(|f| f.name == "y").expect("field y");
    assert_eq!(y.descriptor, "Ljava/lang/String;");
    assert!(y.access & krusty::codegen::classfile::ACC_FINAL == 0, "var backing field must not be final");

    // Constructor + accessors.
    assert!(ci.method("<init>", "(ILjava/lang/String;)V").is_some(), "primary constructor");
    assert!(ci.method("getX", "()I").is_some(), "getX");
    assert!(ci.method("getY", "()Ljava/lang/String;").is_some(), "getY");
    assert!(ci.method("setY", "(Ljava/lang/String;)V").is_some(), "setY for var");
    // val property has no setter.
    assert!(ci.method("setX", "(I)V").is_none(), "val must not have a setter");
}

#[test]
fn member_function_shape_and_run() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping member_function_shape_and_run: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }

    let src = "class Calc(val base: Int) {\n  fun addTo(n: Int): Int = base + n\n}";
    let bytes = compile_class(src, "Calc", "Calc");
    let ci = parse_class(&bytes).expect("parse");
    // Member function is a public (non-static) instance method with the derived JVM descriptor.
    let m = ci.method("addTo", "(I)I").expect("addTo(I)I");
    assert!(!m.is_static(), "member function must be an instance method");

    let dir = std::env::temp_dir().join(format!("krusty_mfn_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Calc.class"), &bytes).unwrap();
    let main = r#"
public class Main {
    public static void main(String[] a) {
        Calc c = new Calc(10);
        System.out.println(c.addTo(5));
    }
}"#;
    fs::write(dir.join("Main.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("Main.java")).output().expect("javac");
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"]).output().expect("java");
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "15\n");
    let _ = fs::remove_dir_all(&dir);
}

/// Compile the class, then a Java `Main` that constructs it and calls the accessors; run under
/// `-Xverify:all` so the JVM verifier validates the constructor/getter bytecode.
#[test]
fn class_loads_verifies_and_runs() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping class_loads_verifies_and_runs: set JAVA_HOME or KRUSTY_REF_JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        eprintln!("skipping: no javac at {javac}");
        return;
    }

    let dir = std::env::temp_dir().join(format!("krusty_class_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let bytes = compile_class("class Point(val x: Int, var y: String)", "Point", "Point");
    fs::write(dir.join("Point.class"), &bytes).unwrap();

    let main = r#"
public class Main {
    public static void main(String[] a) {
        Point p = new Point(7, "hi");
        p.setY("bye");
        System.out.println(p.getX() + ":" + p.getY());
    }
}
"#;
    fs::write(dir.join("Main.java"), main).unwrap();
    let jc = Command::new(&javac)
        .args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()])
        .arg(dir.join("Main.java"))
        .output()
        .expect("run javac");
    assert!(jc.status.success(), "javac failed: {}", String::from_utf8_lossy(&jc.stderr));

    let run = Command::new(&java)
        .args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "Main"])
        .output()
        .expect("run java");
    assert!(run.status.success(), "java failed: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7:bye\n");

    let _ = fs::remove_dir_all(&dir);
}
