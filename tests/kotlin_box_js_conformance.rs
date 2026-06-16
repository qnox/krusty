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

    let (mut scanned, mut lowered, mut ok, mut fail) = (0u32, 0u32, 0u32, 0u32);
    let mut failures = Vec::new();

    for file in &files {
        let src = fs::read_to_string(file).unwrap_or_default();
        if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
            continue;
        }
        if !js_applicable(&src) {
            continue;
        }
        scanned += 1;

        let mut d = DiagSink::new();
        let toks = lex(&src, &mut d);
        let ast = parse(&src, &toks, &mut d);
        if d.has_errors() {
            continue;
        }
        let files1 = vec![ast];
        let syms = collect_signatures(&files1, &mut d);
        let info = check_file(&files1[0], &syms, &mut d);
        if d.has_errors() {
            continue;
        }
        let Some(ir) = lower_file(&files1[0], &info, &syms) else { continue };
        lowered += 1;

        let mut js = krusty::js::emit_file(&ir);
        js.push_str("\nconst r = box(); process.stdout.write(String(r));\n");
        let path = work.join("t.js");
        fs::write(&path, &js).unwrap();
        let run = Command::new("node").arg(&path).output().unwrap();
        let out = String::from_utf8_lossy(&run.stdout);
        if run.status.success() && out.trim() == "OK" {
            ok += 1;
        } else {
            fail += 1;
            if failures.len() < 20 {
                failures.push(format!("{}: out={:?} err={}", file.display(), out.trim(), String::from_utf8_lossy(&run.stderr).trim()));
            }
        }
    }
    let _ = fs::remove_dir_all(&work);

    println!("JS backend — scanned(JS-applicable): {scanned}  | IR-lowered: {lowered}  | box()=OK: {ok}  | FAIL: {fail}");
    for f in &failures {
        println!("  FAIL {f}");
    }
    assert_eq!(fail, 0, "JS backend produced miscompiles");
}
