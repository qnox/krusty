//! (v0) `enum class`: entries as `public static final` fields, private `(String,int)` constructor,
//! `<clinit>`, extends `java/lang/Enum`. Entry access (`Color.RED`), `==`, `.name`/`.ordinal`.
//! Compiled by krusty, run on a real JVM.

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
fn enum_shape() {
    let src = "enum class Color { RED, GREEN, BLUE }";
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
    let ci = parse_class(&emit_class(&cd, &files[0], &info, "Color", "Color", &syms, &mut d).0).unwrap();
    assert_eq!(ci.super_class.as_deref(), Some("java/lang/Enum"));
    for e in ["RED", "GREEN", "BLUE"] {
        assert!(ci.fields.iter().any(|f| f.name == e && f.descriptor == "LColor;"), "entry field {e}");
    }
    assert!(ci.method("<init>", "(Ljava/lang/String;I)V").is_some(), "enum ctor");
}

#[test]
fn enum_runs() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping enum_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_enum_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("E.kt"),
        "enum class Dir { NORTH, SOUTH }\nfun box(): String {\n  val d = Dir.SOUTH\n  if (d == Dir.NORTH) return \"f0\"\n  if (d.name != \"SOUTH\") return \"f1\"\n  if (d.ordinal != 1) return \"f2\"\n  if (Dir.NORTH.ordinal != 0) return \"f3\"\n  return \"OK\"\n}\n").unwrap();
    let kc = Command::new(krusty).args(["-d", dir.to_str().unwrap()]).arg(dir.join("E.kt")).output().unwrap();
    assert!(kc.status.success(), "krusty: {}", String::from_utf8_lossy(&kc.stderr));
    fs::write(dir.join("M.java"), "public class M { public static void main(String[] a) { System.out.println(EKt.box()); } }").unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK", "stderr={}", String::from_utf8_lossy(&run.stderr));
    let _ = fs::remove_dir_all(&dir);
}
