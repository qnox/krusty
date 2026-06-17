use std::collections::HashMap;
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn first_error(src: &str) -> Option<String> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    if d.has_errors() { return Some(d.diags[0].msg.clone()); }
    let syms = collect_signatures(&files, &mut d);
    if d.has_errors() { return Some(d.diags[0].msg.clone()); }
    check_file(&files[0], &syms, &mut d);
    if d.has_errors() { return Some(d.diags[0].msg.clone()); }
    None
}

fn categorize(err: &str) -> String {
    if err.contains("class bodies support") { return "nested decl in class body".into(); }
    if err.contains("interface default") { return "interface default method".into(); }
    if err.contains("mutable local variable") { return "mutable lambda capture".into(); }
    if err.contains("bridge") { return "bridge method".into(); }
    if err.contains("nullable primitive") || err.ends_with("? is not supported") { return "nullable primitive".into(); }
    if err.contains("value/inline") || err.contains("inline class") { return "value/inline class".into(); }
    if err.contains("secondary constructor") { return "secondary constructor".into(); }
    if err.contains("conflicting declarations") { return "conflicting declarations".into(); }
    if err.contains("krusty: ") {
        let m = err.trim_start_matches("krusty: ");
        return format!("krusty: {}", &m[..m.len().min(60)]);
    }
    if err.contains("expected") {
        return format!("parse: {}", &err[..err.len().min(60)]);
    }
    format!("other: {}", &err[..err.len().min(60)])
}

fn collect_kt(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut es: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        es.sort();
        for p in es {
            if p.is_dir() { collect_kt(&p, out); }
            else if p.extension().map_or(false, |e| e == "kt") { out.push(p); }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let box_dir = args.next().expect("usage: survey <box_dir> [--samples <category>]");
    let samples_cat = if args.next().as_deref() == Some("--samples") { args.next() } else { None };

    let mut errors: HashMap<String, Vec<String>> = HashMap::new();
    let mut scanned = 0u32;
    let mut compiled = 0u32;
    let mut files = Vec::new();
    collect_kt(std::path::Path::new(&box_dir), &mut files);
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        if src.contains("// FILE:") || src.contains("// MODULE:") { continue; }
        if !src.contains("fun box()") { continue; }
        if src.contains("// LAMBDAS: INDY") || src.contains("IGNORE_BACKEND_K2: JVM_IR") { continue; }
        if let Some(tb) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
            let t = tb.trim_start_matches("// TARGET_BACKEND:").trim();
            if !t.split(',').any(|x| matches!(x.trim(), "JVM" | "JVM_IR")) { continue; }
        }
        scanned += 1;
        match first_error(&src) {
            None => compiled += 1,
            Some(e) => {
                let cat = categorize(&e);
                errors.entry(cat).or_default().push(f.to_string_lossy().to_string());
            }
        }
    }
    let total_skip: u32 = errors.values().map(|v| v.len() as u32).sum();
    println!("Scanned: {scanned}  Compiled: {compiled}  Skip-errors: {total_skip}");
    let mut sorted: Vec<_> = errors.iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    if let Some(cat) = &samples_cat {
        for (k, files) in &sorted {
            if k.contains(cat.as_str()) {
                println!("Category: {k} ({} files)", files.len());
                for f in files.iter() {
                    println!("{f}");
                }
            }
        }
    } else {
        for (k, v) in &sorted { println!("  {:4}  {k}", v.len()); }
    }
}
