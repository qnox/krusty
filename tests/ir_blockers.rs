//! Diagnostic: for JVM-applicable single-file `box()` tests that parse+check cleanly but do NOT
//! lower to IR, tally the actual unsupported AST node variants (walking the expr/stmt arenas) — an
//! accurate roadmap of what to add to `ir_lower` next. Gated on KRUSTY_KOTLIN_BOX_DIR.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use krusty::ast::{Expr, ExprId, File, Stmt, StmtId};
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

/// Names of AST expr/stmt variants the IR lowering does NOT yet handle (the rest are supported).
fn unsupported(file: &File) -> Vec<&'static str> {
    let mut out = Vec::new();
    for id in 0..file.expr_arena.len() {
        let name = match file.expr(ExprId(id as u32)) {
            Expr::Lambda { .. } => "Expr::Lambda",
            Expr::Elvis { .. } => "Expr::Elvis",
            Expr::NotNull { .. } => "Expr::NotNull(!!)",
            Expr::SafeCall { .. } => "Expr::SafeCall(?.)",
            Expr::CallableRef { .. } => "Expr::CallableRef(::)",
            Expr::Try { .. } => "Expr::Try",
            Expr::Throw { .. } => "Expr::Throw",
            Expr::NullLit => "Expr::NullLit",
            _ => continue,
        };
        out.push(name);
    }
    for id in 0..file.stmt_arena.len() {
        let name = match file.stmt(StmtId(id as u32)) {
            Stmt::Destructure { .. } => "Stmt::Destructure",
            Stmt::IncDec { .. } => "Stmt::IncDec(++/--)",
            Stmt::LocalFun(_) => "Stmt::LocalFun",
            Stmt::AssignMember { .. } => "Stmt::AssignMember",
            _ => continue,
        };
        out.push(name);
    }
    out
}

#[test]
fn ir_blockers() {
    std::thread::Builder::new().stack_size(512 * 1024 * 1024).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let Ok(box_dir) = std::env::var("KRUSTY_KOTLIN_BOX_DIR") else { return; };
    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);

    let (mut total, mut lowered, mut nearmiss) = (0u32, 0u32, 0u32);
    // count of files that contain each unsupported variant (once per file), and files with classes/decls.
    let mut tally: BTreeMap<&str, u32> = BTreeMap::new();
    let mut clean_but_unlowered = 0u32; // no unsupported expr/stmt variant — blocked by a decl-level feature
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
            nearmiss += 1;
            let mut us: Vec<&str> = unsupported(&f1[0]);
            us.sort();
            us.dedup();
            if us.is_empty() {
                clean_but_unlowered += 1;
            }
            for n in us {
                *tally.entry(n).or_default() += 1;
            }
        }
    }
    let mut v: Vec<_> = tally.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    println!("box(JVM): {total}  | IR-lowered: {lowered}  | parse+check OK but NOT lowered: {nearmiss}");
    println!("  of those, {clean_but_unlowered} have NO unsupported expr/stmt (blocked by a decl-level feature: class shape, inheritance, lambdas-as-args resolved elsewhere, etc.)");
    println!("unsupported expr/stmt node variants, by # of near-miss files containing them:");
    for (name, n) in v {
        println!("  {n:5}  {name}");
    }
}
