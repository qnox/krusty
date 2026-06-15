//! `Char` type + char literals: comparison/equality (int instructions), concat (`append(C)`),
//! `toString` (`valueOf(C)`), char-typed params/returns. Run on a real JVM; ABI vs kotlinc.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::codegen::emit::{emit_file, file_class_name};
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
fun first(): Char = 'K'
fun isVowel(c: Char): Boolean = c == 'a' || c == 'e'
fun label(c: Char): String = "[" + c + "]"
fun box(): String {
    if (first() != 'K') return "f1"
    if (!isVowel('a')) return "f2"
    if (isVowel('z')) return "f3"
    if (label('x') != "[x]") return "f4"
    if ('a' < 'b' == false) return "f5"
    return "OK"
}
"#;

#[test]
fn char_runs_correctly() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping char_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_char_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let internal = file_class_name("C", None);
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
fn char_abi_matches_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping char_abi: set KRUSTY_KOTLINC");
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_charabi_{}", std::process::id()));
    let kr = root.join("kr");
    let refd = root.join("ref");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&kr).unwrap();
    fs::create_dir_all(&refd).unwrap();
    fs::write(kr.join("CKt.class"), compile(SRC, "CKt")).unwrap();
    fs::write(root.join("C.kt"), SRC).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("C.kt")).args(["-d", refd.to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    assert!(cmd.output().expect("kotlinc").status.success());
    assert_eq!(abi(&kr, "CKt"), abi(&refd, "CKt"), "char ABI differs from kotlinc");
    let _ = fs::remove_dir_all(&root);
}
