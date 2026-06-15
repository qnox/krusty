//! Differential test vs the real kotlinc: compile the same source with krusty and with kotlinc,
//! then compare (1) the public ABI signatures (javap) and (2) runtime behavior (execution).
//!
//! Opt-in via env (so the suite stays green without a kotlinc install):
//!   KRUSTY_KOTLINC        path to a kotlinc launcher (e.g. .../kotlinc/bin/kotlinc)
//!   KRUSTY_REF_JAVA_HOME  JDK used to RUN kotlinc (older kotlinc needs <= JDK 21)
//!   KRUSTY_KOTLIN_STDLIB  path to kotlin-stdlib.jar (to run kotlinc's output)

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::codegen::emit::emit_file;
use krusty::diag::DiagSink;
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
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    let (bytes, _) = emit_file(&files[0], &info, &syms, internal, &mut d);
    assert!(!d.has_errors(), "krusty errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
    bytes
}

/// Public `static` method signatures from `javap -p <class>` in a normalized, order-independent set.
fn abi_signatures(dir: &PathBuf, class: &str) -> BTreeSet<String> {
    let out = Command::new("javap").args(["-p", "-cp", dir.to_str().unwrap(), class]).output().expect("javap");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(|l| l.trim())
        .filter(|l| l.contains('(') && l.contains("static"))
        .map(|l| l.trim_end_matches(';').split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn abi_and_execution_match_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping diff_kotlinc: set KRUSTY_KOTLINC to enable");
        return;
    };

    // Subset that krusty + kotlinc both support; same source compiled by both.
    let src = r#"
fun add(a: Int, b: Int): Int = a + b
fun precedence(a: Int, b: Int, c: Int): Int = a + b * c
fun div(a: Int, b: Int): Int = a / b
fun promote(a: Long, b: Int): Long = a + b
fun mixed(a: Double, b: Int): Double = a * b + 1
fun max(a: Int, b: Int): Int = if (a > b) a else b
fun both(a: Int, b: Int): Boolean = a > 0 && b > 0
fun greet(name: String): String = "hi " + name
fun tail(s: String): String = s.substring(1)
fun mid(s: String): String = s.substring(1, 3)
fun find(s: String): Int = s.indexOf("b")
"#;

    let root = std::env::temp_dir().join(format!("krusty_diff_{}", std::process::id()));
    let kr = root.join("kr");
    let refd = root.join("ref");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&kr).unwrap();
    fs::create_dir_all(&refd).unwrap();

    // krusty output
    fs::write(kr.join("DiffKt.class"), krusty_compile(src, "DiffKt")).unwrap();

    // kotlinc reference output (file Diff.kt -> class DiffKt)
    fs::write(root.join("Diff.kt"), src).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Diff.kt")).args(["-d", refd.to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc");
    assert!(kc.status.success(), "kotlinc failed: {}", String::from_utf8_lossy(&kc.stderr));

    // (1) ABI signatures must match exactly.
    let kr_abi = abi_signatures(&kr, "DiffKt");
    let ref_abi = abi_signatures(&refd, "DiffKt");
    assert_eq!(
        kr_abi, ref_abi,
        "\nABI mismatch.\n krusty: {kr_abi:#?}\n kotlinc: {ref_abi:#?}"
    );
    assert!(!kr_abi.is_empty(), "no signatures extracted");

    // (2) Execution behavior must match: drive both with the same Main.
    let main = r#"
public class Main {
    public static void main(String[] x) {
        System.out.println(DiffKt.add(3,4));
        System.out.println(DiffKt.precedence(2,3,4));
        System.out.println(DiffKt.div(7,2));
        System.out.println(DiffKt.promote(5L,3));
        System.out.println(DiffKt.mixed(2.5,4));
        System.out.println(DiffKt.max(9,2));
        System.out.println(DiffKt.both(1,1));
        System.out.println(DiffKt.greet("bob"));
        System.out.println(DiffKt.tail("abcd"));
        System.out.println(DiffKt.mid("abcd"));
        System.out.println(DiffKt.find("abc"));
    }
}"#;
    fs::write(root.join("Main.java"), main).unwrap();

    let run_with = |classdir: &PathBuf, extra_cp: &str| -> String {
        let cp = if extra_cp.is_empty() {
            classdir.to_str().unwrap().to_string()
        } else {
            format!("{}:{}", classdir.to_str().unwrap(), extra_cp)
        };
        // compile Main against this class dir, then run
        let mc = Command::new("javac").args(["-cp", &cp, "Main.java"]).current_dir(&root).output().expect("javac");
        assert!(mc.status.success(), "javac(Main) failed for {cp}: {}", String::from_utf8_lossy(&mc.stderr));
        let r = Command::new("java").args(["-cp", &format!("{}:{}", root.to_str().unwrap(), cp), "Main"]).output().expect("java");
        assert!(r.status.success(), "run failed for {cp}: {}", String::from_utf8_lossy(&r.stderr));
        String::from_utf8_lossy(&r.stdout).into_owned()
    };

    let kr_out = run_with(&kr, "");
    let stdlib = env("KRUSTY_KOTLIN_STDLIB").unwrap_or_default();
    let ref_out = run_with(&refd, &stdlib);
    assert_eq!(kr_out, ref_out, "execution output differs:\n krusty:\n{kr_out}\n kotlinc:\n{ref_out}");

    let _ = fs::remove_dir_all(&root);
}
