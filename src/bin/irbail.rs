//! In-process IR-lowering bail tally (untracked dev tool). One process, no classpath scan — fast.
//! Iterates a box dir, runs the frontend, and counts which files `lower_file` bails on (set
//! KRUSTY_IR_DEBUG=1 to see the per-construct reason via ir_lower's eprintln).
use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "kt") {
                out.push(p);
            }
        }
    }
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: irbail <box_dir>");
    let mut files = Vec::new();
    collect(Path::new(&dir), &mut files);
    files.sort();
    let (mut scanned, mut frontend_ok, mut lowered) = (0u32, 0u32, 0u32);
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
            continue;
        }
        scanned += 1;
        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let files1 = vec![parse(&src, &toks, &mut d)];
        if d.has_errors() {
            continue;
        }
        let mut syms = collect_signatures(&files1, &mut d);
        let info = check_file(&files1[0], &mut syms, &mut d);
        if d.has_errors() {
            continue;
        }
        frontend_ok += 1;
        if lower_file(&files1[0], &info, &syms).is_some() {
            lowered += 1;
        }
    }
    eprintln!(
        "DONE scanned={scanned} frontend_ok={frontend_ok} lowered={lowered} bailed={}",
        frontend_ok - lowered
    );
}
