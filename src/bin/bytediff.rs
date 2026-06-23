//! Normalized bytecode differential: krusty vs the real kotlinc.
//!
//! The `box()=OK` conformance gate proves *runtime* correctness; this tool measures the project's
//! harder goal — emitting the SAME bytecode kotlinc does. It compiles each box-corpus file with both
//! compilers, then compares per-class disassembly (`javap -c -p`) after normalizing away the noise
//! that differs without changing semantics (constant-pool indices, bytecode offsets). Two classes that
//! normalize equal have the same method signatures and the same instruction sequences.
//!
//! Opt-in (slow: one kotlinc JVM launch per file) — NOT part of the <60s test gate.
//!
//! Usage:
//!   KRUSTY_KOTLINC=<kotlinc> KRUSTY_SURVEY_STDLIB=<kotlin-stdlib.jar> \
//!   KRUSTY_SURVEY_JDK_MODULES=<JDK/lib/modules> JAVA_HOME=<jdk> \
//!   cargo run --release --bin bytediff -- <box_dir> [limit] [--samples]

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::names::file_class_name;
use krusty::jvm::value_classes::lower_value_classes;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

/// Compile one source with krusty's full pipeline → `(internal_name, class_bytes)` list, or `None` if
/// krusty can't compile it (then there's nothing to diff).
fn krusty_compile(src: &str, stem: &str, cp: &Rc<Classpath>) -> Option<Vec<(String, Vec<u8>)>> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    if d.has_errors() {
        return None;
    }
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return None;
    }
    let info = check_file(&files[0], &syms, &mut d);
    if d.has_errors() {
        return None;
    }
    let facade = file_class_name(stem, files[0].package.as_deref());
    let mut ir = lower_file(&files[0], &info, &syms)?;
    if !lower_value_classes(&mut ir) {
        return None;
    }
    if !krusty::jvm::suspend::lower_suspend(&mut ir) {
        return None;
    }
    let out = emit_all(&ir, &facade, &**cp)?;
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Normalize `javap -c -p` output so semantically-equal bytecode compares equal: drop the source-file
/// banner, the per-instruction bytecode offset (`  12: `), and constant-pool index tokens (`#21`) —
/// keeping the access flags, descriptors, instruction mnemonics, operands, and javap's resolved
/// `// Method …`/`// String …` comments (the semantic content the pool index points at).
fn normalize_javap(out: &str) -> String {
    let mut lines = Vec::new();
    for raw in out.lines() {
        let line = raw.trim_end();
        if line.starts_with("Compiled from") || line.is_empty() {
            continue;
        }
        // Strip a leading "<spaces><digits>: " bytecode-offset prefix on a code line.
        let trimmed = line.trim_start();
        let body = match trimmed.find(": ") {
            Some(pos) if trimmed[..pos].chars().all(|c| c.is_ascii_digit()) && pos > 0 => {
                &trimmed[pos + 2..]
            }
            _ => trimmed,
        };
        // Remove `#<digits>` constant-pool index tokens (e.g. `invokevirtual #21  // …` → `invokevirtual  // …`).
        let mut cleaned = String::with_capacity(body.len());
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                cleaned.push(bytes[i] as char);
                i += 1;
            }
        }
        // Collapse runs of whitespace so offset/pool removal doesn't leave ragged gaps.
        let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            lines.push(normalized);
        }
    }
    lines.join("\n")
}

fn javap(java_home: &str, class_file: &Path) -> Option<String> {
    let out = Command::new(format!("{java_home}/bin/javap"))
        .args(["-c", "-p"])
        .arg(class_file)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(normalize_javap(&String::from_utf8_lossy(&out.stdout)))
}

fn write_class(dir: &Path, internal: &str, bytes: &[u8]) -> Option<PathBuf> {
    let path = dir.join(format!("{internal}.class"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

fn collect_kt(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut es: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        es.sort();
        for p in es {
            if p.is_dir() {
                collect_kt(&p, out);
            } else if p.extension().is_some_and(|e| e == "kt") {
                out.push(p);
            }
        }
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let box_dir = args
        .next()
        .expect("usage: bytediff <box_dir> [limit] [--samples]");
    let mut limit = usize::MAX;
    let mut show_samples = false;
    for a in args {
        if a == "--samples" {
            show_samples = true;
        } else if let Ok(n) = a.parse::<usize>() {
            limit = n;
        }
    }

    let kotlinc = env("KRUSTY_KOTLINC").expect("set KRUSTY_KOTLINC");
    let stdlib = env("KRUSTY_SURVEY_STDLIB").expect("set KRUSTY_SURVEY_STDLIB");
    let java_home = env("JAVA_HOME")
        .or_else(|| env("KRUSTY_REF_JAVA_HOME"))
        .expect("set JAVA_HOME");

    let mut cp_paths: Vec<PathBuf> = vec![PathBuf::from(&stdlib)];
    if let Some(m) = env("KRUSTY_SURVEY_JDK_MODULES") {
        cp_paths.push(PathBuf::from(m));
    }
    let cp = Rc::new(Classpath::new(cp_paths));

    let tmp = std::env::temp_dir().join(format!("krusty_bytediff_{}", std::process::id()));
    let kdir = tmp.join("krusty");
    let rdir = tmp.join("ref");

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);

    let (mut files_diffed, mut cls_total, mut cls_equal, mut cls_missing) =
        (0u32, 0u32, 0u32, 0u32);
    let mut sample_diffs: Vec<(String, String)> = Vec::new(); // (class file, first differing line pair)

    for f in &files {
        if files_diffed >= limit as u32 {
            break;
        }
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let src = src.replace("OPTIONAL_JVM_INLINE_ANNOTATION", "@JvmInline");
        if src.contains("// FILE:")
            || src.contains("// MODULE:")
            || !src.contains("fun box()")
            || src.contains("// LAMBDAS: INDY")
            || src.contains("IGNORE_BACKEND_K2: JVM_IR")
        {
            continue;
        }
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("File");
        let Some(krusty_classes) = krusty_compile(&src, stem, &cp) else {
            continue; // krusty can't compile it — not a bytecode diff, it's a coverage gap
        };

        // Compile the same source with the real kotlinc.
        let _ = std::fs::remove_dir_all(&rdir);
        let _ = std::fs::create_dir_all(&rdir);
        let _ = std::fs::remove_dir_all(&kdir);
        let _ = std::fs::create_dir_all(&kdir);
        let kc = Command::new(&kotlinc)
            .arg(f)
            .args(["-d", rdir.to_str().unwrap(), "-classpath", &stdlib])
            .output();
        match kc {
            Ok(o) if o.status.success() => {}
            _ => continue, // kotlinc rejected it (directive/feature) — nothing to compare against
        }

        files_diffed += 1;
        for (internal, bytes) in &krusty_classes {
            let Some(kpath) = write_class(&kdir, internal, bytes) else {
                continue;
            };
            let rpath = rdir.join(format!("{internal}.class"));
            cls_total += 1;
            if !rpath.exists() {
                cls_missing += 1; // krusty emitted a class kotlinc didn't (a structural divergence)
                continue;
            }
            let (Some(kn), Some(rn)) = (javap(&java_home, &kpath), javap(&java_home, &rpath))
            else {
                continue;
            };
            if kn == rn {
                cls_equal += 1;
            } else if show_samples && sample_diffs.len() < 20 {
                // First differing normalized line, for a quick eyeball of where they diverge.
                let first = kn
                    .lines()
                    .zip(rn.lines())
                    .find(|(a, b)| a != b)
                    .map(|(a, b)| format!("krusty: {a}\n  ref : {b}"))
                    .unwrap_or_else(|| "(length differs)".to_string());
                sample_diffs.push((format!("{}::{internal}", f.display()), first));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&tmp);

    let pct = if cls_total > 0 {
        100.0 * cls_equal as f64 / cls_total as f64
    } else {
        0.0
    };
    println!("=== krusty vs kotlinc normalized bytecode diff ===");
    println!(
        "files compiled by both: {files_diffed}  | classes compared: {cls_total}  | byte-identical (normalized): {cls_equal} ({pct:.1}%)  | krusty-only classes: {cls_missing}"
    );
    if show_samples {
        println!("\n--- first divergence per differing class (up to 20) ---");
        for (cls, diff) in &sample_diffs {
            println!("{cls}\n  {diff}");
        }
    }
}
