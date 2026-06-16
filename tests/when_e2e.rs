//! `when` expressions (subject + subjectless forms, comma conditions, `else`) and reference `==`.
//! Compiled by krusty and run on a real JVM; ABI is diffed against kotlinc when available.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::jvm::emit::{emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn compile(src: &str, internal: &str) -> Vec<u8> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let file = parse(src, &toks, &mut d);
    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

const SRC: &str = r#"
fun grade(n: Int): Int = when (n) { 0 -> 10; 1, 2 -> 20; else -> 99 }
fun sign(n: Int): Int = when { n < 0 -> -1; n > 0 -> 1; else -> 0 }
fun label(s: String): Int = when (s) { "a" -> 1; "bb" -> 2; else -> 0 }
fun eqs(a: String, b: String): Boolean = a == b
fun box(): String {
    if (grade(0) != 10) return "f1"
    if (grade(2) != 20) return "f2"
    if (grade(5) != 99) return "f3"
    if (sign(-3) != -1) return "f4"
    if (sign(8) != 1) return "f5"
    if (label("bb") != 2) return "f6"
    if (label("z") != 0) return "f7"
    if (!eqs("x", "x")) return "f8"
    if (eqs("x", "y")) return "f9"
    return "OK"
}
"#;

#[test]
fn when_runs_correctly_on_jvm() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping when_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_when_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let internal = file_class_name("When", None);
    fs::write(dir.join(format!("{internal}.class")), compile(SRC, &internal)).unwrap();
    let main = format!("public class M {{ public static void main(String[] a) {{ System.out.println({internal}.box()); }} }}");
    fs::write(dir.join("M.java"), main).unwrap();
    let jc = Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    assert!(jc.status.success(), "javac: {}", String::from_utf8_lossy(&jc.stderr));
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert!(run.status.success(), "java: {}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "OK");
    let _ = fs::remove_dir_all(&dir);
}

fn abi(dir: &PathBuf, class: &str) -> BTreeSet<String> {
    let out = Command::new("javap").args(["-p", "-cp", dir.to_str().unwrap(), class]).output().expect("javap");
    String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim())
        .filter(|l| l.contains('(') && l.contains("static"))
        .map(|l| l.trim_end_matches(';').split_whitespace().collect::<Vec<_>>().join(" ")).collect()
}

#[test]
fn when_abi_matches_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping when_abi: set KRUSTY_KOTLINC");
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_whenabi_{}", std::process::id()));
    let kr = root.join("kr");
    let refd = root.join("ref");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&kr).unwrap();
    fs::create_dir_all(&refd).unwrap();
    fs::write(kr.join("WhenKt.class"), compile(SRC, "WhenKt")).unwrap();
    fs::write(root.join("When.kt"), SRC).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("When.kt")).args(["-d", refd.to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    assert!(cmd.output().expect("kotlinc").status.success());
    assert_eq!(abi(&kr, "WhenKt"), abi(&refd, "WhenKt"), "when ABI differs from kotlinc");
    let _ = fs::remove_dir_all(&root);
}
