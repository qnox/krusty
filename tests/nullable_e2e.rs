//! Nullable reference types: `T?`, `null`, `== null`/`!= null`, `!!` (not-null assertion), and
//! `?:` (elvis). Compiled by krusty and run on a real JVM; ABI diffed against kotlinc.

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
fun orDefault(s: String?): String = s ?: "default"
fun firstOrNull(b: Boolean): String? = if (b) "yes" else null
fun force(s: String?): String = s!!
fun isNil(s: String?): Boolean = s == null
fun box(): String {
    if (orDefault(null) != "default") return "f1"
    if (orDefault("x") != "x") return "f2"
    if (firstOrNull(false) != null) return "f3"
    if (force("ok") != "ok") return "f4"
    if (!isNil(null)) return "f5"
    if (isNil("a")) return "f6"
    return "OK"
}
"#;

#[test]
fn nullable_runs_correctly() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        eprintln!("skipping nullable_e2e: set JAVA_HOME");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_null_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let internal = file_class_name("N", None);
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

#[test]
fn not_null_assertion_throws_on_null() {
    let Ok(java_home) = std::env::var("KRUSTY_REF_JAVA_HOME").or_else(|_| std::env::var("JAVA_HOME")) else {
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("krusty_null2_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("NKt.class"), compile("fun force(s: String?): String = s!!\n", "NKt")).unwrap();
    let main = "public class M { public static void main(String[] a) { try { NKt.force(null); System.out.println(\"NOFAIL\"); } catch (NullPointerException e) { System.out.println(\"NPE\"); } } }";
    fs::write(dir.join("M.java"), main).unwrap();
    assert!(Command::new(&javac).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap().status.success());
    let run = Command::new(&java).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "NPE", "!! must throw NPE on null");
    let _ = fs::remove_dir_all(&dir);
}

fn abi(dir: &PathBuf, class: &str) -> BTreeSet<String> {
    let out = Command::new("javap").args(["-p", "-cp", dir.to_str().unwrap(), class]).output().expect("javap");
    String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim())
        .filter(|l| l.contains('(') && l.contains("static"))
        .map(|l| l.trim_end_matches(';').split_whitespace().collect::<Vec<_>>().join(" ")).collect()
}

#[test]
fn nullable_abi_matches_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping nullable_abi: set KRUSTY_KOTLINC");
        return;
    };
    let root = std::env::temp_dir().join(format!("krusty_nullabi_{}", std::process::id()));
    let kr = root.join("kr");
    let refd = root.join("ref");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&kr).unwrap();
    fs::create_dir_all(&refd).unwrap();
    fs::write(kr.join("NKt.class"), compile(SRC, "NKt")).unwrap();
    fs::write(root.join("N.kt"), SRC).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("N.kt")).args(["-d", refd.to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    assert!(cmd.output().expect("kotlinc").status.success());
    assert_eq!(abi(&kr, "NKt"), abi(&refd, "NKt"), "nullable ABI differs from kotlinc");
    let _ = fs::remove_dir_all(&root);
}
