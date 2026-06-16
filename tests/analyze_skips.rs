use std::collections::{HashMap, BTreeMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;

use krusty::codegen::emit::{emit_class, emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

fn collect_kt(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect_kt(&p, out);
            } else if p.extension().map_or(false, |e| e == "kt") {
                out.push(p);
            }
        }
    }
}

/// Compile and return (success, full_error_message)
fn compile_with_error(src: &str, stem: &str) -> (bool, String) {
    let mut diags = DiagSink::new();
    
    let toks = lex(src, &mut diags);
    let files = vec![parse(src, &toks, &mut diags)];
    
    if diags.has_errors() {
        let msg = diags.diags.first().map(|d| d.msg.clone()).unwrap_or_default();
        return (false, msg);
    }
    
    let syms = collect_signatures(&files, &mut diags);
    if diags.has_errors() {
        let msg = diags.diags.first().map(|d| d.msg.clone()).unwrap_or_default();
        return (false, msg);
    }
    
    let file = &files[0];
    let info = check_file(file, &syms, &mut diags);
    if diags.has_errors() {
        let msg = diags.diags.first().map(|d| d.msg.clone()).unwrap_or_default();
        return (false, msg);
    }

    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new();
    let facade_name = file_class_name(stem, file.package.as_deref());

    for &d in &file.decls {
        if let krusty::ast::Decl::Class(c) = file.decl(d) {
            let internal = match file.package.as_deref() {
                Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), c.name),
                _ => c.name.clone(),
            };
            let (bytes, extra) = emit_class(c, &file, &info, &internal, &facade_name, &syms, &mut diags);
            if diags.has_errors() {
                let msg = diags.diags.first().map(|d| d.msg.clone()).unwrap_or_default();
                return (false, msg);
            }
            outputs.push((internal, bytes));
            outputs.extend(extra);
        }
    }

    let has_facade = file.decls.iter().any(|&d| {
        matches!(file.decl(d), krusty::ast::Decl::Fun(_) | krusty::ast::Decl::Property(_))
    });
    if has_facade {
        let internal = file_class_name(stem, file.package.as_deref());
        let (bytes, extra) = emit_file(&file, &info, &syms, &internal, &mut diags);
        if diags.has_errors() {
            let msg = diags.diags.first().map(|d| d.msg.clone()).unwrap_or_default();
            return (false, msg);
        }
        outputs.push((internal, bytes));
        outputs.extend(extra);
    }

    if outputs.is_empty() {
        return (false, "no output generated".to_string());
    }
    (true, String::new())
}

#[test]
fn analyze_skip_reasons() {
    let Some(box_dir) = std::env::var("KRUSTY_KOTLIN_BOX_DIR").ok().filter(|v| !v.is_empty()) else {
        eprintln!("skipping: set KRUSTY_KOTLIN_BOX_DIR");
        return;
    };

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);
    files.truncate(1200); // Sample first 1200

    let error_counts: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());
    let error_examples: Mutex<HashMap<String, Vec<(PathBuf, String)>>> = Mutex::new(HashMap::new());
    let compiled = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);

    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(8 * 1024 * 1024)
        .build()
        .unwrap();

    pool.install(|| {
        files.par_iter().for_each(|file| {
            let src = match fs::read_to_string(file) {
                Ok(s) => s,
                Err(_) => return,
            };

            // Apply test filters
            if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
                return;
            }
            if src.contains("// LAMBDAS: INDY") || src.contains("IGNORE_BACKEND_K2: JVM_IR") {
                return;
            }
            if let Some(tb_line) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
                let targets = tb_line.trim_start_matches("// TARGET_BACKEND:").trim();
                if !targets.split(',').any(|t| matches!(t.trim(), "JVM" | "JVM_IR")) {
                    return;
                }
            }

            let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("File").to_string();
            let (success, error) = compile_with_error(&src, &stem);

            if success {
                compiled.fetch_add(1, Ordering::Relaxed);
            } else {
                skipped.fetch_add(1, Ordering::Relaxed);
                
                // Categorize: take full message up to first clause/phrase boundary
                let key = categorize_error(&error);

                let mut counts = error_counts.lock().unwrap();
                *counts.entry(key.clone()).or_insert(0) += 1;
                drop(counts);

                let mut examples = error_examples.lock().unwrap();
                examples.entry(key).or_insert_with(Vec::new).push((file.clone(), error.clone()));
            }
        });
    });

    let compiled_val = compiled.load(Ordering::Relaxed);
    let skipped_val = skipped.load(Ordering::Relaxed);

    eprintln!("\n=== Analyzed ~{} tests ===", compiled_val + skipped_val);
    eprintln!("Compiled: {}  Skipped: {}\n", compiled_val, skipped_val);

    let counts = error_counts.lock().unwrap();
    let examples = error_examples.lock().unwrap();

    eprintln!("=== Top 30 Skip Reasons (by frequency) ===");
    let mut sorted: Vec<_> = counts.iter().collect();
    sorted.sort_by_key(|&(_, count)| std::cmp::Reverse(*count));

    for (reason, count) in sorted.iter().take(30) {
        eprintln!("{count:4}x: {reason}");
        if let Some(exs) = examples.get(*reason) {
            for (f, msg) in exs.iter().take(2) {
                let rel = f.strip_prefix(&box_dir).unwrap_or(f);
                eprintln!("      ex: {} -> {}", rel.display(), msg);
            }
        }
    }
}

fn categorize_error(err: &str) -> String {
    // Extract the key error message, stripping details and normalizing
    let trimmed = err.trim();
    
    // Take first 60 chars or up to first line break
    let short = if let Some(idx) = trimmed.find('\n') {
        &trimmed[..idx]
    } else {
        trimmed
    };
    
    // Truncate if very long, keep the essence
    if short.len() > 70 {
        format!("{}...", &short[..67])
    } else {
        short.to_string()
    }
}
