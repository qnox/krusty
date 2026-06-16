//! `object` declarations (singletons): `public static final INSTANCE` + private constructor +
//! member functions, built in `<clinit>`; `Obj.member(args)` lowers to getstatic INSTANCE +
//! invokevirtual. Compiled by krusty, run on a real JVM; consumed by the real kotlinc.

use std::fs;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::emit_class;
use krusty::diag::DiagSink;
use krusty::jvm::classreader::parse_class;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn object_shape_is_singleton() {
    let src = "object Counter { fun inc(n: Int): Int = n + 1 }";
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let cd = files[0].decls.iter().find_map(|&id| match files[0].decl(id) {
        Decl::Class(c) => Some(c.clone()),
        _ => None,
    }).unwrap();
    let ci = parse_class(&emit_class(&cd, &files[0], &info, "Counter", "Counter", &syms, &mut d).0).unwrap();
    let inst = ci.fields.iter().find(|f| f.name == "INSTANCE").expect("INSTANCE field");
    assert_eq!(inst.descriptor, "LCounter;");
    assert!(ci.method("inc", "(I)I").is_some());
    let ctor = ci.method("<init>", "()V").expect("ctor");
    assert!(ctor.access & krusty::jvm::classfile::ACC_PRIVATE != 0, "object ctor must be private");
}

#[test]
fn object_runs_and_round_trips() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping object_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let root = std::env::temp_dir().join(format!("krusty_obj_{}", std::process::id()));
    let lib = root.join("lib");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("O.kt"), "package demo\nobject Math2 { fun sq(n: Int): Int = n * n }\nfun box(): String = if (Math2.sq(6) == 36) \"OK\" else \"fail\"\n").unwrap();
    let kc = Command::new(krusty).args(["-d", lib.to_str().unwrap()]).arg(root.join("O.kt")).output().unwrap();
    assert!(kc.status.success(), "krusty: {}", String::from_utf8_lossy(&kc.stderr));

    let main = "public class M { public static void main(String[] a) { System.out.println(demo.OKt.box()); } }";
    fs::write(root.join("M.java"), main).unwrap();
    assert!(Command::new(&javac).args(["-cp", lib.to_str().unwrap(), "-d", lib.to_str().unwrap()]).arg(root.join("M.java")).output().unwrap().status.success());
    let run = Command::new(&java).args(["-Xverify:all", "-cp", lib.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&run.stderr));

    if let Some(kotlinc) = env("KRUSTY_KOTLINC") {
        fs::write(root.join("C.kt"), "import demo.Math2\nfun main() { println(Math2.sq(7)) }\n").unwrap();
        let mut cmd = Command::new(&kotlinc);
        cmd.arg(root.join("C.kt")).args(["-cp", lib.to_str().unwrap(), "-d", root.join("cout").to_str().unwrap()]);
        cmd.env("JAVA_HOME", &java_home);
        assert!(cmd.output().unwrap().status.success(), "kotlinc failed to consume krusty object");
    }
    let _ = fs::remove_dir_all(&root);
}
