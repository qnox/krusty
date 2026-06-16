//! Diagnostic: for JVM-applicable single-file `box()` tests that parse+check cleanly but do NOT
//! lower to IR, tally which language features appear — to prioritize what to add to `ir_lower`.
//! Gated on KRUSTY_KOTLIN_BOX_DIR.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn collect_kt(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { collect_kt(&p, out); } else if p.extension().map_or(false, |x| x == "kt") { out.push(p); }
    }
}

#[test]
fn ir_blockers() {
    std::thread::Builder::new().stack_size(512 * 1024 * 1024).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let Ok(box_dir) = std::env::var("KRUSTY_KOTLIN_BOX_DIR") else { return; };
    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);

    // (feature, contains-predicate)
    let feats: &[(&str, fn(&str) -> bool)] = &[
        ("WITH_STDLIB", |s| s.contains("// WITH_STDLIB") || s.contains("// WITH_RUNTIME")),
        ("data class", |s| s.contains("data class")),
        ("object decl", |s| s.contains("object ") || s.contains("object{")),
        ("enum class", |s| s.contains("enum class")),
        ("interface", |s| s.contains("interface ")),
        ("lambda { }", |s| s.contains("->") && s.contains("{")),
        ("nullable ?", |s| s.contains("?")),
        ("inheritance :", |s| s.contains(") :") || s.contains("> :")),
        ("generics <>", |s| s.matches('<').count() > 0 && s.contains("fun ") && s.contains('<')),
        ("is/as", |s| s.contains(" is ") || s.contains(" as ")),
        ("try/catch", |s| s.contains("try")),
        ("when-is", |s| s.contains("when") && s.contains(" is ")),
        ("inline fun", |s| s.contains("inline fun")),
        ("companion", |s| s.contains("companion")),
        ("extension fun", |s| s.contains("fun ") && (s.contains(".") )),
    ];

    let (mut total, mut lowered, mut nearmiss) = (0u32, 0u32, 0u32);
    let mut tally: BTreeMap<&str, u32> = BTreeMap::new();
    for file in &files {
        let src = fs::read_to_string(file).unwrap_or_default();
        if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
            continue;
        }
        if let Some(l) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
            if !l.split(',').any(|t| t.contains("JVM")) { continue; }
        }
        total += 1;
        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let f1 = vec![parse(&src, &toks, &mut d)];
        if d.has_errors() { continue; }
        let syms = collect_signatures(&f1, &mut d);
        let info = check_file(&f1[0], &syms, &mut d);
        if d.has_errors() { continue; }
        if lower_file(&f1[0], &info, &syms).is_some() {
            lowered += 1;
        } else {
            // parses+checks but doesn't lower: a near-miss the IR subset can't yet handle
            nearmiss += 1;
            for (name, pred) in feats {
                if pred(&src) { *tally.entry(*name).or_default() += 1; }
            }
        }
    }
    let mut v: Vec<_> = tally.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    println!("box(JVM): {total}  | IR-lowered: {lowered}  | parse+check OK but NOT lowered: {nearmiss}");
    println!("features present in the non-lowered (parse+check OK) files:");
    for (name, n) in v {
        println!("  {n:5}  {name}");
    }
}
