//! Throwaway analysis: bucket first-error failures by the source line at the error span.
use std::collections::HashMap;
use krusty::diag::{DiagSink, line_col};
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn first_error(src: &str) -> Option<(String, u32)> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    if d.has_errors() { let e = &d.diags[0]; return Some((e.msg.clone(), e.span.lo)); }
    let syms = collect_signatures(&files, &mut d);
    if d.has_errors() { let e = &d.diags[0]; return Some((e.msg.clone(), e.span.lo)); }
    check_file(&files[0], &syms, &mut d);
    if d.has_errors() { let e = &d.diags[0]; return Some((e.msg.clone(), e.span.lo)); }
    None
}

fn collect_kt(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() { collect_kt(&p, out); }
            else if p.extension().map_or(false, |e| e == "kt") { out.push(p); }
        }
    }
}

fn norm(line: &str) -> String {
    // Normalize a source line into a coarse pattern: drop identifiers/strings/numbers.
    let mut out = String::new();
    let mut prev_space = false;
    for ch in line.trim().chars() {
        let c = if ch.is_alphanumeric() || ch == '_' { 'X' } else { ch };
        if c == 'X' && out.ends_with('X') { continue; }
        if c == ' ' { if prev_space { continue; } prev_space = true; } else { prev_space = false; }
        out.push(c);
    }
    out.chars().take(50).collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let box_dir = args.next().expect("usage: blockers <box_dir> [msg-substring]");
    let filter = args.next();
    let mut files = Vec::new();
    collect_kt(std::path::Path::new(&box_dir), &mut files);
    let mut buckets: HashMap<String, (u32, Vec<String>)> = HashMap::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        if src.contains("// FILE:") || src.contains("// MODULE:") { continue; }
        if !src.contains("fun box()") { continue; }
        if let Some(tb) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
            let t = tb.trim_start_matches("// TARGET_BACKEND:").trim();
            if !t.split(',').any(|x| matches!(x.trim(), "JVM" | "JVM_IR")) { continue; }
        }
        if let Some((msg, off)) = first_error(&src) {
            if let Some(flt) = &filter { if !msg.contains(flt.as_str()) { continue; } }
            let (line, _) = line_col(&src, off);
            let src_line = src.lines().nth(line - 1).unwrap_or("");
            let key = norm(src_line);
            let e = buckets.entry(key).or_default();
            e.0 += 1;
            if e.1.len() < 2 { e.1.push(format!("{}: {}", f.file_name().unwrap().to_string_lossy(), src_line.trim())); }
        }
    }
    let mut sorted: Vec<_> = buckets.into_iter().collect();
    sorted.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (k, (n, samples)) in sorted.iter().take(40) {
        println!("{:4}  {:50}", n, k);
        for s in samples { println!("        {}", &s[..s.len().min(90)]); }
    }
}
