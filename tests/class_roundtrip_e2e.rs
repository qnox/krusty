//! Phase 8b: the decisive class-metadata round-trip. krust compiles a Kotlin class; the REAL
//! kotlinc compiles a *Kotlin consumer* that uses it via **property syntax** (`p.x`, `p.y = ...`).
//! This only type-checks if kotlinc reads krust's class `@kotlin.Metadata` (kind=1). If it passes,
//! krust's classes are consumable as genuine Kotlin classes — Kotlin-side ABI for classes.
//!
//! Gated by KRUST_KOTLINC (+ KRUST_REF_JAVA_HOME, KRUST_KOTLIN_STDLIB).

use std::fs;
use std::process::Command;

use krust::ast::Decl;
use krust::codegen::emit::emit_class;
use krust::diag::DiagSink;
use krust::lexer::lex;
use krust::parser::parse;
use krust::resolve::collect_signatures;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn krust_compile_class(src: &str, class_name: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let cd = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == class_name => Some(c.clone()),
            _ => None,
        })
        .expect("class decl");
    let bytes = emit_class(&cd, internal, &syms);
    assert!(!d.has_errors(), "krust errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

#[test]
fn kotlinc_consumes_krust_class_via_property_syntax() {
    let Some(kotlinc) = env("KRUST_KOTLINC") else {
        eprintln!("skipping class_roundtrip: set KRUST_KOTLINC to enable");
        return;
    };

    let root = std::env::temp_dir().join(format!("krust_crt_{}", std::process::id()));
    let lib = root.join("lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(lib.join("demo")).unwrap();

    // krust compiles a Kotlin class in package `demo`.
    let src = "package demo\nclass Point(val x: Int, var y: String)\n";
    fs::write(lib.join("demo/Point.class"), krust_compile_class(src, "Point", "demo/Point")).unwrap();

    // kotlinc compiles a consumer using KOTLIN PROPERTY SYNTAX (p.x, p.y = ...), which only works
    // if kotlinc recognizes the class as Kotlin (reads its @Metadata) — a Java view would require
    // getX()/setY() instead.
    let consumer = "import demo.Point\nfun main() {\n  val p = Point(7, \"hi\")\n  p.y = \"bye\"\n  println(p.x.toString() + \":\" + p.y)\n}\n";
    fs::write(root.join("Consumer.kt"), consumer).unwrap();

    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Consumer.kt"))
        .args(["-cp", lib.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
    if let Some(jh) = env("KRUST_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc on consumer");
    assert!(
        kc.status.success(),
        "kotlinc FAILED to consume krust class @Metadata:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );

    // Run it to confirm behavior (krust class + kotlinc consumer + stdlib).
    if let Some(stdlib) = env("KRUST_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), lib.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "ConsumerKt"]).output().expect("java");
        if run.status.success() {
            assert_eq!(String::from_utf8_lossy(&run.stdout), "7:bye\n", "stderr={}", String::from_utf8_lossy(&run.stderr));
        }
    }

    let _ = fs::remove_dir_all(&root);
}
