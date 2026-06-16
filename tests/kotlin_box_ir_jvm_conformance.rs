//! IR→JVM conformance against the real Kotlin box corpus. For each JVM-applicable single-file
//! `fun box()` test in the IR core subset, lower AST→`krusty-ir`→JVM bytecode via `ir_emit` (NOT the
//! AST emitter), run it on a real JVM, and assert `box() == "OK"`. This measures how much of the
//! corpus the *IR pipeline* already compiles correctly on the JVM — the precursor to routing the
//! JVM box path through `ir_emit` and retiring `emit.rs`. As `ir_lower` grows, this count rises.
//!
//! Gated: `KRUSTY_KOTLIN_BOX_DIR` + a JDK (`JAVA_HOME`/`KRUSTY_REF_JAVA_HOME`). No-ops otherwise.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn collect_kt(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_kt(&p, out);
        } else if p.extension().map_or(false, |x| x == "kt") {
            out.push(p);
        }
    }
}

fn jvm_applicable(src: &str) -> bool {
    let names = ["JVM", "JVM_IR"];
    let mentions = |line: &str| line.split(',').any(|t| names.contains(&t.trim()));
    if let Some(l) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
        if !mentions(l.trim_start_matches("// TARGET_BACKEND:").trim()) {
            return false;
        }
    }
    for l in src.lines().filter(|l| l.starts_with("// IGNORE_BACKEND")) {
        if mentions(l.splitn(2, ':').nth(1).unwrap_or("").trim()) {
            return false;
        }
    }
    true
}

/// Unsigned types/literals — krusty maps them to signed Int/Long (no unsigned model), so any
/// program that depends on unsigned semantics or widths is out of scope.
fn uses_unsigned(src: &str) -> bool {
    if src.contains("UInt") || src.contains("ULong") || src.contains("UByte") || src.contains("UShort") {
        return true;
    }
    // unsigned literal suffix: a digit followed by `u`/`U` (optionally then `L`).
    let bytes = src.as_bytes();
    for i in 1..bytes.len() {
        if (bytes[i] == b'u' || bytes[i] == b'U') && bytes[i - 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

enum Outcome {
    Skip,
    Ok,
    Fail(String),
}

#[test]
fn kotlin_codegen_box_ir_jvm_conformance() {
    let Ok(box_dir) = std::env::var("KRUSTY_KOTLIN_BOX_DIR") else {
        eprintln!("skipping IR/JVM box conformance: set KRUSTY_KOTLIN_BOX_DIR");
        return;
    };
    let Some(jh) = std::env::var("KRUSTY_REF_JAVA_HOME").ok().or_else(|| std::env::var("JAVA_HOME").ok()) else {
        return;
    };
    let javac = format!("{jh}/bin/javac");
    if !Path::new(&javac).exists() {
        return;
    }

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);
    let limit: usize = std::env::var("KRUSTY_BOX_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    files.truncate(limit);

    let work = std::env::temp_dir().join(format!("krusty_irjvm_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    use rayon::prelude::*;
    let pool = rayon::ThreadPoolBuilder::new().stack_size(256 * 1024 * 1024).build().unwrap();
    let results: Vec<Outcome> = pool.install(|| {
        files.par_iter().enumerate().map(|(i, f)| run_one(f, i, &work, &jh)).collect()
    });
    let _ = fs::remove_dir_all(&work);

    let lowered = results.iter().filter(|o| !matches!(o, Outcome::Skip)).count();
    let ok = results.iter().filter(|o| matches!(o, Outcome::Ok)).count();
    let failures: Vec<&String> = results.iter().filter_map(|o| if let Outcome::Fail(s) = o { Some(s) } else { None }).collect();

    println!("IR->JVM backend — IR-lowered: {lowered}  | box()=OK: {ok}  | FAIL: {}", failures.len());
    for f in failures.iter().take(20) {
        println!("  FAIL {f}");
    }
    assert_eq!(failures.len(), 0, "IR->JVM produced miscompiles");
}

fn run_one(file: &Path, idx: usize, work: &Path, jh: &str) -> Outcome {
    let src = fs::read_to_string(file).unwrap_or_default();
    if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") || !jvm_applicable(&src) {
        return Outcome::Skip;
    }
    // krusty does not model unsigned types — skip (the AST emitter's harness skips them too).
    if uses_unsigned(&src) {
        return Outcome::Skip;
    }
    let mut d = DiagSink::new();
    let toks = lex(&src, &mut d);
    let files1 = vec![parse(&src, &toks, &mut d)];
    if d.has_errors() {
        return Outcome::Skip;
    }
    let syms = collect_signatures(&files1, &mut d);
    let info = check_file(&files1[0], &syms, &mut d);
    if d.has_errors() {
        return Outcome::Skip;
    }
    let Some(ir) = lower_file(&files1[0], &info, &syms) else { return Outcome::Skip };

    let dir = work.join(format!("t{idx}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let classes = krusty::jvm::ir_emit::emit_all(&ir, "BoxKt");
    // Find the facade that has box(): it's BoxKt.
    for (n, b) in &classes {
        let path = dir.join(format!("{n}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b).unwrap();
    }
    fs::write(dir.join("M.java"), "public class M{public static void main(String[] a){System.out.println(BoxKt.box());}}").unwrap();
    let jc = Command::new(format!("{jh}/bin/javac")).args(["-cp", dir.to_str().unwrap(), "-d", dir.to_str().unwrap()]).arg(dir.join("M.java")).output().unwrap();
    if !jc.status.success() {
        return Outcome::Fail(format!("{}: javac: {}", file.display(), String::from_utf8_lossy(&jc.stderr).lines().next().unwrap_or("")));
    }
    let r = Command::new(format!("{jh}/bin/java")).args(["-Xverify:all", "-cp", dir.to_str().unwrap(), "M"]).output().unwrap();
    let out = String::from_utf8_lossy(&r.stdout);
    if r.status.success() && out.trim() == "OK" {
        Outcome::Ok
    } else {
        Outcome::Fail(format!("{}: out={:?} err={}", file.display(), out.trim(), String::from_utf8_lossy(&r.stderr).lines().next().unwrap_or("")))
    }
}
