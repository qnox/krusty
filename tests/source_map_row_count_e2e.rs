//! A one-line inline body's rows in the `SourceDebugExtension` (SMAP) map.
//!
//! JSR-045 omits a row's repeat count and output-line increment when they are 1, and the reference
//! compiler follows that in both strata. krusty always wrote those values, so every class that
//! expands a one-line `inline fun` carried two rows kotlinc spells differently:
//!
//! ```text
//! kotlinc:  *L          krusty:  *L
//!           1#2:3                1#2,1:3
//!           …                    …
//!           1#1:3                1#1:3,1
//! ```
//!
//! An ordinary `inline fun f(): Int = …` reaches it — its whole body is one line — so this is not
//! an edge case reserved for generated code.
use std::fs;

use super::common;

const LIBRARY: &str = "inline fun twice(f: () -> Int): Int = f() + f()\n";
const MAIN: &str = "fun g(): Int = twice { 21 }\n";

/// The `SourceDebugExtension` payload of `MainKt` as both compilers write it, or `None` when the
/// reference toolchain is unavailable.
fn source_maps() -> Option<(String, String)> {
    let _jh = common::java_home(); // panics with the JAVA_HOME diagnosis when absent
    let library = common::compile_lib_ref("smap_row_count", LIBRARY)?;
    let dir = std::env::temp_dir().join(format!("krusty_smap_rows_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    fs::create_dir_all(&reference_dir).unwrap();
    fs::create_dir_all(&krusty_dir).unwrap();
    let source = dir.join("Main.kt");
    fs::write(&source, MAIN).unwrap();
    let (code, stderr) = common::kotlinc_compile(&[
        "-classpath".to_string(),
        library.to_string_lossy().into_owned(),
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_metadata_cp(
        MAIN,
        "Main",
        &[library.clone(), common::stdlib_jar(), jdk],
    )
    .expect("krusty failed to compile the one-line inline fixture");
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == "MainKt")
        .expect("krusty did not emit MainKt");
    fs::write(krusty_dir.join("MainKt.class"), bytes).unwrap();

    let both = (
        common::source_debug_extension(&reference_dir.join("MainKt.class")).join("\n"),
        common::source_debug_extension(&krusty_dir.join("MainKt.class")).join("\n"),
    );
    let _ = fs::remove_dir_all(&dir);
    Some(both)
}

#[test]
fn a_one_line_inline_body_writes_no_repeat_count() {
    let Some((reference, krusty)) = source_maps() else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    // Both strata must be in the reference, or the comparison below proves nothing about either.
    assert_eq!(
        reference.lines().collect::<Vec<_>>(),
        vec![
            "SMAP",
            "Main.kt",
            "Kotlin",
            "*S Kotlin",
            "*F",
            "+ 1 Main.kt",
            "MainKt",
            "+ 2 Lib.kt",
            "LibKt",
            "*L",
            "1#1,2:1",
            "1#2:3",
            "*S KotlinDebug",
            "*F",
            "+ 1 Main.kt",
            "MainKt",
            "*L",
            "1#1:3",
            "*E",
        ],
        "the reference map, spelled out so a reference change is visible here"
    );
    assert_eq!(krusty, reference, "SourceDebugExtension");
}
