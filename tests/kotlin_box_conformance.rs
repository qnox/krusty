//! Ports the Kotlin compiler's canonical conformance suite (`compiler/testData/codegen/box`, the
//! `fun box(): String` → `"OK"` tests) and runs every case through krusty:
//!
//!   * **skip**  — krusty can't compile it yet (uses a feature outside the supported subset);
//!   * **pass**  — krusty compiles it and `box()` returns `"OK"` on a real JVM;
//!   * **FAIL**  — krusty *accepted* the program but produced wrong/invalid bytecode (a real bug).
//!
//! The test fails only on a FAIL (krusty must never miscompile a case it accepts); pass/skip counts
//! are reported so coverage is visible and grows automatically as the language widens.
//!
//! Gated by env (the suite is huge and lives in the Kotlin source tree):
//!   KRUSTY_KOTLIN_BOX_DIR  path to compiler/testData/codegen/box
//!   KRUSTY_REF_JAVA_HOME / JAVA_HOME  a JDK to run box()
//!   KRUSTY_KOTLIN_STDLIB   kotlin-stdlib.jar (on the run classpath, just in case)
//!   KRUSTY_BOX_LIMIT       cap on number of files scanned (default 4000)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::jvm::classreader::parse_class;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

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

/// Find a compiled class exposing a static `box()Ljava/lang/String;` and return its binary name.
fn find_box_class(out_dir: &Path) -> Option<String> {
    fn walk(dir: &Path, found: &mut Option<String>) {
        if found.is_some() {
            return;
        }
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, found);
                } else if p.extension().map_or(false, |x| x == "class") {
                    if let Ok(ci) = parse_class(&fs::read(&p).unwrap_or_default()) {
                        if ci.method("box", "()Ljava/lang/String;").map_or(false, |m| m.is_static()) {
                            *found = Some(ci.this_class.replace('/', "."));
                            return;
                        }
                    }
                }
            }
        }
    }
    let mut found = None;
    walk(out_dir, &mut found);
    found
}

#[test]
fn kotlin_codegen_box_conformance() {
    let Some(box_dir) = env("KRUSTY_KOTLIN_BOX_DIR") else {
        eprintln!("skipping box conformance: set KRUSTY_KOTLIN_BOX_DIR to compiler/testData/codegen/box");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping box conformance: set JAVA_HOME to run box()");
        return;
    };
    let javac = format!("{java_home}/bin/javac");
    let java = format!("{java_home}/bin/java");
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let stdlib = env("KRUSTY_KOTLIN_STDLIB").unwrap_or_default();
    let limit: usize = env("KRUSTY_BOX_LIMIT").and_then(|v| v.parse().ok()).unwrap_or(4000);

    let mut files = Vec::new();
    collect_kt(Path::new(&box_dir), &mut files);
    files.truncate(limit);

    let work = std::env::temp_dir().join(format!("krusty_box_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    let (mut compiled, mut passed, mut skipped) = (0usize, 0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();

    for (i, file) in files.iter().enumerate() {
        let src = fs::read_to_string(file).unwrap_or_default();
        // Multi-file (`// FILE:`) and multi-module (`// MODULE:`) tests are out of scope for the
        // single-translation-unit driver.
        if src.contains("// FILE:") || src.contains("// MODULE:") || !src.contains("fun box()") {
            skipped += 1;
            continue;
        }
        let out = work.join(format!("o{i}"));
        let _ = fs::create_dir_all(&out);

        // krusty compiles it? Non-zero exit ⇒ unsupported feature ⇒ skip.
        let kc = Command::new(krusty).args(["-d", out.to_str().unwrap()]).arg(file).output();
        let Ok(kc) = kc else { skipped += 1; continue };
        if !kc.status.success() {
            skipped += 1;
            let _ = fs::remove_dir_all(&out);
            continue;
        }
        compiled += 1;

        let Some(box_class) = find_box_class(&out) else {
            skipped += 1;
            let _ = fs::remove_dir_all(&out);
            continue;
        };

        // Run box() via a tiny Java Main; krusty accepted it, so it MUST verify and return "OK".
        let main = format!("public class M {{ public static void main(String[] a) {{ System.out.println({box_class}.box()); }} }}");
        fs::write(out.join("M.java"), main).unwrap();
        let jc = Command::new(&javac).args(["-cp", out.to_str().unwrap(), "-d", out.to_str().unwrap()]).arg(out.join("M.java")).output().unwrap();
        if !jc.status.success() {
            failures.push(format!("{}: javac(Main) failed: {}", file.display(), String::from_utf8_lossy(&jc.stderr).lines().next().unwrap_or("")));
            let _ = fs::remove_dir_all(&out);
            continue;
        }
        let cp = if stdlib.is_empty() { out.to_str().unwrap().to_string() } else { format!("{}:{}", out.to_str().unwrap(), stdlib) };
        let run = Command::new(&java).args(["-cp", &cp, "M"]).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        // `box()` may print to stdout itself; its return value is what Main prints LAST.
        let returned = stdout.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").trim();
        if run.status.success() && returned == "OK" {
            passed += 1;
        } else {
            let why = if run.status.success() { format!("box()={returned:?}") } else { String::from_utf8_lossy(&run.stderr).lines().next().unwrap_or("run failed").to_string() };
            failures.push(format!("{}: {why}", file.display()));
        }
        let _ = fs::remove_dir_all(&out);
    }

    let _ = fs::remove_dir_all(&work);
    eprintln!("\n=== Kotlin codegen/box conformance ===");
    eprintln!("scanned: {}  | krusty-compiled: {compiled}  | box()=OK: {passed}  | skipped(unsupported): {skipped}  | FAIL: {}", files.len(), failures.len());
    for f in failures.iter().take(25) {
        eprintln!("  FAIL {f}");
    }
    assert!(failures.is_empty(), "{} box case(s) were accepted by krusty but miscompiled (see above)", failures.len());
    assert!(passed > 0, "no box() cases ran — check KRUSTY_KOTLIN_BOX_DIR / JDK");
}
