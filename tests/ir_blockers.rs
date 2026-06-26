//! Diagnostic: for JVM-applicable single-file `box()` tests that parse+check cleanly but do NOT
//! lower to IR, tally the actual unsupported AST node variants (walking the expr/stmt arenas) — an
//! accurate roadmap of what to add to `ir_lower` next. Gated on KRUSTY_KOTLIN_BOX_DIR.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use krusty::ast::{Decl, Expr, ExprId, File, FunBody, Stmt, StmtId};
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
        } else if p.extension().is_some_and(|x| x == "kt") {
            out.push(p);
        }
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
            _ => continue,
        };
        out.push(name);
    }
    out
}

/// Decl-level reasons a file falls outside the IR subset (why `lower_file` bails before any expr) —
/// the breakdown of the "no unsupported expr/stmt" bucket. Mirrors `is_simple_class`/`lower_file`.
fn decl_blockers(file: &File) -> Vec<&'static str> {
    let mut out = Vec::new();
    for id in 0..file.decl_arena.len() {
        match file.decl(krusty::ast::DeclId(id as u32)) {
            Decl::Fun(f) => {
                if f.receiver.is_some() {
                    out.push("fun: extension receiver");
                }
                if f.is_inline {
                    out.push("fun: inline");
                }
            }
            Decl::Class(c) => {
                if c.is_data {
                    out.push("class: data");
                }
                if c.is_object() {
                    out.push("class: object");
                }
                if c.is_enum() {
                    out.push("class: enum");
                }
                if c.is_interface() {
                    out.push("class: interface");
                }
                if c.is_abstract() {
                    out.push("class: abstract");
                }
                if c.is_open() {
                    out.push("class: open");
                }
                if c.base_class.is_some() {
                    out.push("class: base class");
                }
                if !c.supertypes.is_empty() {
                    out.push("class: supertypes");
                }
                if !c.body_props.is_empty() {
                    out.push("class: body properties");
                }
                if !c.companion_methods.is_empty() {
                    out.push("class: companion");
                }
                if !c.secondary_ctors.is_empty() {
                    out.push("class: secondary ctor");
                }
                if !c.init_order.is_empty() {
                    out.push("class: init block");
                }
                if c.props.iter().any(|p| !p.is_property) {
                    out.push("class: ctor non-property param");
                }
                if c.methods.iter().any(|m| m.receiver.is_some()) {
                    out.push("class: method receiver");
                }
                if c.methods
                    .iter()
                    .any(|m| !matches!(m.body, FunBody::Expr(_)))
                {
                    out.push("class: block-body method");
                }
            }
            Decl::Property(_) => out.push("top-level property"),
            _ => out.push("other top-level decl"),
        }
    }
    out
}

#[test]
fn ir_blockers() {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let Ok(box_dir) = std::env::var("KRUSTY_KOTLIN_BOX_DIR") else {
        return;
    };
    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);

    let (mut total, mut lowered, mut nearmiss) = (0u32, 0u32, 0u32);
    // count of files that contain each unsupported variant (once per file), and files with classes/decls.
    let mut tally: BTreeMap<&str, u32> = BTreeMap::new();
    let mut decl_tally: BTreeMap<&str, u32> = BTreeMap::new();
    let mut clean_but_unlowered = 0u32; // no unsupported expr/stmt variant — blocked by a decl-level feature
    for file in &files {
        let src = fs::read_to_string(file).unwrap_or_default();
        if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
            continue;
        }
        if let Some(l) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
            if !l.split(',').any(|t| t.contains("JVM")) {
                continue;
            }
        }
        total += 1;
        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let f1 = vec![parse(&src, &toks, &mut d)];
        if d.has_errors() {
            continue;
        }
        let syms = collect_signatures(&f1, &mut d);
        let info = check_file(&f1[0], &syms, &mut d);
        if d.has_errors() {
            continue;
        }
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
            let mut db = decl_blockers(&f1[0]);
            db.sort();
            db.dedup();
            for n in db {
                *decl_tally.entry(n).or_default() += 1;
            }
        }
    }
    let mut v: Vec<_> = tally.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    println!(
        "box(JVM): {total}  | IR-lowered: {lowered}  | parse+check OK but NOT lowered: {nearmiss}"
    );
    println!("  of those, {clean_but_unlowered} have NO unsupported expr/stmt (blocked by a decl-level feature: class shape, inheritance, lambdas-as-args resolved elsewhere, etc.)");
    println!("unsupported expr/stmt node variants, by # of near-miss files containing them:");
    for (name, n) in v {
        println!("  {n:5}  {name}");
    }
    let mut dv: Vec<_> = decl_tally.into_iter().collect();
    dv.sort_by(|a, b| b.1.cmp(&a.1));
    println!("decl-level blockers, by # of near-miss files containing them:");
    for (name, n) in dv {
        println!("  {n:5}  {name}");
    }
}
