//! Differential ABI test vs kotlinc for a property-holding class: krusty and kotlinc compile the
//! same `class Point(val x, var y)`; their public member signatures (constructor + accessors, via
//! javap) must match exactly, and both must construct + run identically.
//!
//! Opt-in via env (same as diff_kotlinc.rs):
//!   KRUSTY_KOTLINC, KRUSTY_REF_JAVA_HOME, KRUSTY_KOTLIN_STDLIB

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use krusty::ast::Decl;
use krusty::jvm::emit::emit_class;
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

/// Public member signatures (constructor + methods) from `javap`, order-independent and normalized.
fn member_signatures(dir: &PathBuf, class: &str) -> BTreeSet<String> {
    let out = Command::new("javap").args(["-cp", dir.to_str().unwrap(), class]).output().expect("javap");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(|l| l.trim())
        .filter(|l| l.contains('(') && !l.contains("class "))
        .map(|l| l.trim_end_matches(';').split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn class_abi_and_execution_match_kotlinc() {
    let Some(kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping diff_class_kotlinc: set KRUSTY_KOTLINC to enable");
        return;
    };

    let src = "class Point(val x: Int, var y: String)\n";

    let root = std::env::temp_dir().join(format!("krusty_cdiff_{}", std::process::id()));
    let kr = root.join("kr");
    let refd = root.join("ref");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&kr).unwrap();
    fs::create_dir_all(&refd).unwrap();

    fs::write(kr.join("Point.class"), krusty_compile_class(src, "Point", "Point")).unwrap();

    fs::write(root.join("Point.kt"), src).unwrap();
    let mut cmd = Command::new(&kotlinc);
    cmd.arg(root.join("Point.kt")).args(["-d", refd.to_str().unwrap()]);
    if let Some(jh) = env("KRUSTY_REF_JAVA_HOME") {
        cmd.env("JAVA_HOME", jh);
    }
    let kc = cmd.output().expect("run kotlinc");
    assert!(kc.status.success(), "kotlinc failed: {}", String::from_utf8_lossy(&kc.stderr));

    let kr_abi = member_signatures(&kr, "Point");
    let ref_abi = member_signatures(&refd, "Point");
    assert_eq!(kr_abi, ref_abi, "\nclass ABI mismatch.\n krusty: {kr_abi:#?}\n kotlinc: {ref_abi:#?}");
    assert!(!kr_abi.is_empty(), "no signatures extracted");

    // Execution: both classes drive the same Main identically.
    let main = r#"
public class Main {
    public static void main(String[] a) {
        Point p = new Point(7, "hi");
        System.out.println(p.getX() + ":" + p.getY());
        p.setY("bye");
        System.out.println(p.getY());
    }
}"#;
    fs::write(root.join("Main.java"), main).unwrap();

    let run_with = |classdir: &PathBuf, extra_cp: &str| -> String {
        let cp = if extra_cp.is_empty() {
            classdir.to_str().unwrap().to_string()
        } else {
            format!("{}:{}", classdir.to_str().unwrap(), extra_cp)
        };
        let mc = Command::new("javac").args(["-cp", &cp, "Main.java"]).current_dir(&root).output().expect("javac");
        assert!(mc.status.success(), "javac(Main) failed: {}", String::from_utf8_lossy(&mc.stderr));
        let r = Command::new("java").args(["-cp", &format!("{}:{}", root.to_str().unwrap(), cp), "Main"]).output().expect("java");
        assert!(r.status.success(), "run failed for {cp}: {}", String::from_utf8_lossy(&r.stderr));
        String::from_utf8_lossy(&r.stdout).into_owned()
    };

    let kr_out = run_with(&kr, "");
    let stdlib = env("KRUSTY_KOTLIN_STDLIB").unwrap_or_default();
    let ref_out = run_with(&refd, &stdlib);
    assert_eq!(kr_out, ref_out, "execution differs:\n krusty:\n{kr_out}\n kotlinc:\n{ref_out}");

    let _ = fs::remove_dir_all(&root);
}
