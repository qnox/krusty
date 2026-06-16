//! JS-backend conformance against the **same** Kotlin box corpus the JVM harness uses. For each
//! single-file `fun box()` test that targets JS (per `// TARGET_BACKEND:` / `// IGNORE_BACKEND:`)
//! and lies inside the IR core subset, lower AST→`krusty-ir`→JavaScript and run it on `node`,
//! asserting `box() === "OK"`. The IR subset is small today, so most files are skipped at lowering
//! (no node spawn); this grows automatically as `ir_lower` covers more constructs.
//!
//! Gated: `KRUSTY_KOTLIN_BOX_DIR` must point at `compiler/testData/codegen/box`, and `node` must be
//! on `PATH`. Without them the test no-ops (so the suite stays green offline).

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

/// Respect kotlinc backend directives for the JS backend (mirror of the JVM harness helper).
fn js_applicable(src: &str) -> bool {
    let names = ["JS", "JS_IR"];
    let mentions = |line: &str| line.split(',').any(|t| names.contains(&t.trim()));
    if let Some(l) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
        if !mentions(l.trim_start_matches("// TARGET_BACKEND:").trim()) {
            return false;
        }
    }
    for l in src.lines().filter(|l| l.starts_with("// IGNORE_BACKEND")) {
        let rest = l.splitn(2, ':').nth(1).unwrap_or("");
        if mentions(rest.trim()) {
            return false;
        }
    }
    true
}

#[test]
fn kotlin_codegen_box_js_conformance() {
    // Some corpus files nest expressions deeply enough to overflow the default 8 MB test stack
    // during parse/lowering — run the scan on a thread with a large stack.
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let Ok(box_dir) = std::env::var("KRUSTY_KOTLIN_BOX_DIR") else {
        eprintln!("skipping JS box conformance: set KRUSTY_KOTLIN_BOX_DIR");
        return;
    };
    if Command::new("node").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping JS box conformance: node not available");
        return;
    }

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);
    let limit: usize = std::env::var("KRUSTY_BOX_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    files.truncate(limit);

    let work = std::env::temp_dir().join(format!("krusty_jsbox_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    use rayon::prelude::*;
    // Parallel: each file is independent (own diag sink, own node input file keyed by index).
    // Big worker stacks — some corpus sources nest deeply enough to overflow a default stack.
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(256 * 1024 * 1024)
        .build()
        .unwrap();
    let results: Vec<Outcome> = pool.install(|| {
        files
            .par_iter()
            .enumerate()
            .map(|(i, file)| run_one(file, i, &work))
            .collect()
    });
    let _ = fs::remove_dir_all(&work);

    let scanned = results.iter().filter(|o| !matches!(o, Outcome::Irrelevant)).count();
    let lowered = results.iter().filter(|o| matches!(o, Outcome::Ok | Outcome::Fail(_))).count();
    let ok = results.iter().filter(|o| matches!(o, Outcome::Ok)).count();
    let failures: Vec<&String> = results.iter().filter_map(|o| match o { Outcome::Fail(s) => Some(s), _ => None }).collect();

    println!("JS backend — scanned(JS-applicable): {scanned}  | IR-lowered: {lowered}  | box()=OK: {ok}  | FAIL: {}", failures.len());
    for f in failures.iter().take(20) {
        println!("  FAIL {f}");
    }
    assert_eq!(failures.len(), 0, "JS backend produced miscompiles");
}

enum Outcome {
    Irrelevant, // not a JS-applicable single-file box test
    Skip,       // applicable but outside the IR core subset (or front-end error)
    Ok,
    Fail(String),
}

fn run_one(file: &Path, idx: usize, work: &Path) -> Outcome {
    let src = fs::read_to_string(file).unwrap_or_default();
    if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") || !js_applicable(&src) {
        return Outcome::Irrelevant;
    }
    let mut d = DiagSink::new();
    let toks = lex(&src, &mut d);
    let ast = parse(&src, &toks, &mut d);
    if d.has_errors() {
        return Outcome::Skip;
    }
    let files1 = vec![ast];
    let syms = collect_signatures(&files1, &mut d);
    let info = check_file(&files1[0], &syms, &mut d);
    if d.has_errors() {
        return Outcome::Skip;
    }
    let Some(ir) = lower_file(&files1[0], &info, &syms) else { return Outcome::Skip };

    let mut js = krusty::js::emit_file(&ir);
    js.push_str("\nconst r = box(); process.stdout.write(String(r));\n");
    let path = work.join(format!("t{idx}.js"));
    fs::write(&path, &js).unwrap();
    let run = Command::new("node").arg(&path).output().unwrap();
    let _ = fs::remove_file(&path);
    let out = String::from_utf8_lossy(&run.stdout);
    if run.status.success() && out.trim() == "OK" {
        Outcome::Ok
    } else {
        Outcome::Fail(format!("{}: out={:?} err={}", file.display(), out.trim(), String::from_utf8_lossy(&run.stderr).trim()))
    }
}
