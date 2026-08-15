//! In-repo conformance: real Kotlin `codegen/box` cases (vendored under `tests/box_data/`) that fall
//! within krusty's supported subset. Each is compiled by the `krusty` binary and its `box(): String`
//! is run on a real JVM; it must return `"OK"`. These run in normal `cargo test` (given a JDK),
//! unlike the full external sweep in `kotlin_box_conformance.rs`.
//!
//! Provenance: copied verbatim from JetBrains/kotlin `compiler/testData/codegen/box/` (Apache-2.0).

use std::fs;
use std::path::Path;

use super::common;

#[test]
fn vendored_kotlin_box_cases_return_ok() {
    // Strict environment: missing JDK/stdlib panics with the provisioning diagnosis (tests/common).
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_data");

    let mut cases: Vec<_> = fs::read_dir(&data)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "kt"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no vendored box cases found");

    // Compile every case in-process (the same pipeline `krusty -d` runs, warm classpath caches) and
    // run its box() on the POOLED runner JVM — per-request classloaders isolate the cases, so no
    // javac/java/krusty process is spawned per case.
    let mut skipped = 0usize;
    let mut accepted = 0usize;
    for kt in &cases {
        let src = fs::read_to_string(kt).unwrap();
        let stem = kt.file_stem().unwrap().to_string_lossy().into_owned();
        let compile_cp = if src.lines().any(|line| line.trim() == "// WITH_STDLIB") {
            std::slice::from_ref(&stdlib)
        } else {
            &[]
        };
        // The IR backend covers a subset; a case it rejects is *skipped*, never a failure. The gate
        // is: every case krusty *accepts* must run and return "OK" (never miscompile an accepted
        // file).
        let Some(classes) =
            common::compile_in_process(&src, &stem, compile_cp, Some(jdk.as_path()))
        else {
            skipped += 1;
            continue;
        };
        let box_class = common::find_box_class(&classes)
            .unwrap_or_else(|| panic!("no box() class for {}", kt.display()));
        let got = common::run_box(&classes, &box_class, std::slice::from_ref(&stdlib))
            .expect("pooled box runner unavailable");
        assert_eq!(got, "OK", "box() did not return OK for {}", kt.display());
        accepted += 1;
    }
    // The corpus is vendored (deterministic per checkout): 2 cases are currently unsupported. A
    // JUMP in skips means the compile path regressed (e.g. a classpath/jdk wiring change silently
    // rejecting cases), which must fail loudly — skips otherwise read as green (see
    // docs/TEST_HARNESS.md on skip semantics).
    assert!(
        skipped <= 2,
        "vendored box skips regressed: {skipped} skipped, {accepted} accepted"
    );
    eprintln!(
        "vendored Kotlin box conformance (IR backend): {accepted} OK, {skipped} skipped (unsupported), {} total",
        cases.len()
    );
}
