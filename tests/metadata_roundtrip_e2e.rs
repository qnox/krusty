//! The decisive `@Metadata` test (Phase 5b): krusty compiles a Kotlin library, then the REAL
//! kotlinc compiles a *Kotlin consumer* against it. For kotlinc to resolve the imported top-level
//! functions it must successfully read krusty's `@kotlin.Metadata`. If this passes, krusty output is
//! consumable as a genuine Kotlin library — the core of Kotlin-side ABI compatibility.
//!
//! Gated by KRUSTY_KOTLINC (+ KRUSTY_REF_JAVA_HOME, KRUSTY_KOTLIN_STDLIB), like diff_kotlinc.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::jvm::emit::emit_file;
use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn krusty_compile(src: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let mut syms = collect_signatures(&files, &mut d);
    syms.classpath = Classpath::empty();
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

#[test]
fn kotlinc_consumes_krusty_metadata() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping metadata_roundtrip: set KRUSTY_KOTLINC to enable");
        return;
    };

    let root = std::env::temp_dir().join(format!("krusty_md_{}", std::process::id()));
    let lib = root.join("lib"); // krusty output: demo/LibKt.class
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(lib.join("demo")).unwrap();

    // krusty compiles a Kotlin library (top-level functions in package `demo`).
    let lib_src = "package demo\nfun greet(name: String): String = \"hi \" + name\nfun addk(a: Int, b: Int): Int = a + b\n";
    fs::write(lib.join("demo/LibKt.class"), krusty_compile(lib_src, "demo/LibKt")).unwrap();
    // The .kotlin_module file maps package `demo` to facade `LibKt` (required for resolution).
    fs::create_dir_all(lib.join("META-INF")).unwrap();
    let module = krusty::metadata::module::build_kotlin_module(&[("demo".into(), vec!["LibKt".into()])]);
    fs::write(lib.join("META-INF/main.kotlin_module"), module).unwrap();

    // The reference kotlinc compiles a Kotlin CONSUMER that imports those functions.
    // This only type-checks if kotlinc successfully reads krusty's @Metadata.
    let consumer = "import demo.greet\nimport demo.addk\nfun main() { println(greet(\"bob\")); println(addk(2, 3)) }\n";
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
        "kotlinc FAILED to consume krusty @Metadata:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );

    // Bonus: run it (krusty lib + kotlinc consumer + stdlib) and check behavior.
    if let Some(stdlib) = env("KRUSTY_KOTLIN_STDLIB") {
        let cp = format!("{}:{}:{}", root.join("cout").to_str().unwrap(), lib.to_str().unwrap(), stdlib);
        let run = Command::new("java").args(["-cp", &cp, "ConsumerKt"]).output().expect("java");
        let out = String::from_utf8_lossy(&run.stdout);
        if run.status.success() {
            assert_eq!(out, "hi bob\n5\n", "consumer output mismatch; stderr={}", String::from_utf8_lossy(&run.stderr));
        }
    }

    let _ = fs::remove_dir_all(&root);
}
