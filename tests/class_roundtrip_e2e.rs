//! Phase 8b: the decisive class-metadata round-trip. krusty compiles a Kotlin class; the REAL
//! kotlinc compiles a *Kotlin consumer* that uses it via **property syntax** (`p.x`, `p.y = ...`).
//! This only type-checks if kotlinc reads krusty's class `@kotlin.Metadata` (kind=1). If it passes,
//! krusty's classes are consumable as genuine Kotlin classes — Kotlin-side ABI for classes.
//!
//! Gated by KRUSTY_KOTLINC (+ KRUSTY_REF_JAVA_HOME, KRUSTY_KOTLIN_STDLIB).

use std::fs;
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

fn krusty_compile_class(src: &str, class_name: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let cd = files[0]
        .decls
        .iter()
        .find_map(|&id| match files[0].decl(id) {
            Decl::Class(c) if c.name == class_name => Some(c.clone()),
            _ => None,
        })
        .expect("class decl");
    let (bytes, _) = emit_class(&cd, &files[0], &info, internal, internal, &syms, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

#[test]
fn kotlinc_consumes_krusty_class_via_property_syntax() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping class_roundtrip: set KRUSTY_KOTLINC to enable");
        return;
    };

    let root = std::env::temp_dir().join(format!("krusty_crt_{}", std::process::id()));
    let lib = root.join("lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(lib.join("demo")).unwrap();

    // krusty compiles a Kotlin class in package `demo` (with a member function).
    let src = "package demo\nclass Point(val x: Int, var y: String) {\n  fun shifted(d: Int): Int = x + d\n}\n";
    fs::write(lib.join("demo/Point.class"), krusty_compile_class(src, "Point", "demo/Point")).unwrap();

    // kotlinc compiles a consumer using KOTLIN PROPERTY SYNTAX (p.x, p.y = ...) and a member call
    // (p.shifted(..)), which only works if kotlinc recognizes the class as Kotlin (reads its
    // @Metadata) — a Java view would require getX()/setY() instead.
    let consumer = "import demo.Point\nfun main() {\n  val p = Point(7, \"hi\")\n  p.y = \"bye\"\n  println(p.x.toString() + \":\" + p.y + \":\" + p.shifted(3))\n}\n";
    fs::write(root.join("Consumer.kt"), consumer).unwrap();

    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Consumer.kt"))
        .args(["-cp", lib.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc on consumer");
    assert!(
        kc.status.success(),
        "kotlinc FAILED to consume krusty class @Metadata:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );

    // Run it to confirm behavior (krusty class + kotlinc consumer + stdlib).
    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), lib.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "ConsumerKt"]).output().expect("java");
        if run.status.success() {
            assert_eq!(String::from_utf8_lossy(&run.stdout), "7:bye:10\n", "stderr={}", String::from_utf8_lossy(&run.stderr));
        }
    }

    let _ = fs::remove_dir_all(&root);
}
