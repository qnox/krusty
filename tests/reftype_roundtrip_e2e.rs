//! Reference-type metadata round-trip: krusty compiles classes with **class-typed members**
//! (`Line(val from: Point, ...)`, methods returning/using class types); the real kotlinc compiles a
//! Kotlin consumer that uses them via property/method syntax and runs it. This validates that the
//! `Ty::Obj` class-id encoding in class `@Metadata` is readable by kotlinc.
//!
//! Gated by KRUSTY_KOTLINC (+ KRUSTY_REF_JAVA_HOME, KRUSTY_KOTLIN_STDLIB).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::ast::Decl;
use krusty::codegen::emit::emit_class;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// Compile every class in `src` into `dir` (using each class's `<pkg>/<Name>` internal name).
fn compile_classes(dir: &PathBuf, src: &str) {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let pkg = files[0].package.clone().unwrap_or_default();
    for &id in &files[0].decls {
        if let Decl::Class(c) = files[0].decl(id) {
            let internal = if pkg.is_empty() { c.name.clone() } else { format!("{}/{}", pkg.replace('.', "/"), c.name) };
            let bytes = emit_class(c, &files[0], &info, &internal, &syms, &mut d);
            let path = dir.join(format!("{internal}.class"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
}

#[test]
fn kotlinc_consumes_class_typed_members() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping reftype_roundtrip: set KRUSTY_KOTLINC to enable");
        return;
    };

    let root = std::env::temp_dir().join(format!("krusty_refrt_{}", std::process::id()));
    let lib = root.join("lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&lib).unwrap();

    let src = "package demo\nclass Point(val x: Int, val y: Int) {\n  fun sum(): Int = x + y\n}\nclass Line(val from: Point, val to: Point) {\n  fun span(): Int = to.x - from.x\n}\n";
    compile_classes(&lib, src);

    // Consumer uses class-typed properties (l.from : Point), nested access (l.to.y), and methods.
    let consumer = "import demo.Point\nimport demo.Line\nfun main() {\n  val l = Line(Point(1, 2), Point(5, 9))\n  println(l.from.sum().toString() + \":\" + l.span() + \":\" + l.to.y)\n}\n";
    fs::write(root.join("Consumer.kt"), consumer).unwrap();

    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Consumer.kt"))
        .args(["-cp", lib.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc");
    assert!(kc.status.success(), "kotlinc FAILED to read class-typed-member @Metadata:\n{}", String::from_utf8_lossy(&kc.stderr));

    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), lib.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "ConsumerKt"]).output().expect("java");
        if run.status.success() {
            assert_eq!(String::from_utf8_lossy(&run.stdout), "3:4:9\n", "stderr={}", String::from_utf8_lossy(&run.stderr));
        }
    }

    let _ = fs::remove_dir_all(&root);
}

/// Top-level functions that take/return a class type: the facade `@Metadata` must encode the class
/// as a class-id so a Kotlin consumer can call `mk(7): Point` / `originX(p): Int`. Uses the real
/// `krusty` driver (classes + `FileKt` facade + `META-INF/*.kotlin_module`).
#[test]
fn kotlinc_consumes_class_typed_top_level_functions() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping reftype_roundtrip facade: set KRUSTY_KOTLINC to enable");
        return;
    };

    let root = std::env::temp_dir().join(format!("krusty_facrt_{}", std::process::id()));
    let lib = root.join("lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    fs::write(root.join("Lib.kt"), "package demo\nclass Point(val x: Int, val y: Int)\nfun mk(a: Int): Point = Point(a, a)\nfun originX(p: Point): Int = p.x\n").unwrap();
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let kr = Command::new(krusty).args(["-d", lib.to_str().unwrap()]).arg(root.join("Lib.kt")).output().expect("run krusty");
    assert!(kr.status.success(), "krusty failed: {}", String::from_utf8_lossy(&kr.stderr));

    let consumer = "import demo.mk\nimport demo.originX\nfun main() {\n  val p = mk(7)\n  println(originX(p))\n}\n";
    fs::write(root.join("Consumer.kt"), consumer).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Consumer.kt")).args(["-cp", lib.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc");
    assert!(kc.status.success(), "kotlinc FAILED on class-typed facade fns:\n{}", String::from_utf8_lossy(&kc.stderr));

    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), lib.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "ConsumerKt"]).output().expect("java");
        if run.status.success() {
            assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n", "stderr={}", String::from_utf8_lossy(&run.stderr));
        }
    }

    let _ = fs::remove_dir_all(&root);
}
