//! A declaration contributed by two classpath entries remains one overload candidate.

use std::path::{Path, PathBuf};

use super::common;

fn copied_stdlib_classpath() -> (PathBuf, [PathBuf; 2]) {
    let directory = common::scratch_dir().expect("allocate copied-classpath fixture");
    let original = common::stdlib_jar();
    let copied = directory.join("kotlin-stdlib-copy.jar");
    std::fs::copy(&original, &copied).expect("copy kotlin stdlib to a distinct classpath path");
    (directory, [original, copied])
}

fn reference_result(source_text: &str, stdlib: &Path, coroutines: &Path) -> String {
    let directory = common::scratch_dir().expect("allocate reference fixture");
    let source = directory.join("Main.kt");
    let output = directory.join("out");
    std::fs::write(&source, source_text).expect("write reference source");
    std::fs::create_dir(&output).expect("create reference output directory");
    let classpath = std::env::join_paths([stdlib, coroutines])
        .expect("join reference classpath")
        .to_string_lossy()
        .into_owned();
    let (code, diagnostics) = common::kotlinc_compile(&[
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-cp".to_string(),
        classpath,
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler available");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    let result = common::run_box(
        &[],
        "MainKt",
        &[
            output,
            stdlib.to_path_buf(),
            coroutines.to_path_buf(),
            common::jdk_modules(),
        ],
    )
    .expect("run kotlinc-built copied-classpath fixture");
    std::fs::remove_dir_all(directory).expect("remove reference fixture");
    result
}

#[test]
fn copied_classpath_entry_keeps_indexed_iteration_unambiguous() {
    const MAIN: &str = r#"
import kotlinx.coroutines.runBlocking

suspend fun twice(value: Int): Int = value * 2

fun box(): String = runBlocking {
    val seen = StringBuilder()
    listOf(10, 20, 30).forEachIndexed { index, value ->
        if (value == 20) return@forEachIndexed
        seen.append(index).append(':').append(twice(value)).append(';')
    }
    if (seen.toString() == "0:20;2:60;") "OK" else "FAIL: $seen"
}
"#;

    let (directory, stdlibs) = copied_stdlib_classpath();
    let coroutines = common::coroutines_jar();
    let jdk = common::jdk_modules();
    let baseline_classpath = [stdlibs[0].clone(), coroutines.clone(), jdk.clone()];
    let duplicated_classpath = [
        stdlibs[0].clone(),
        stdlibs[1].clone(),
        coroutines.clone(),
        jdk.clone(),
    ];
    let baseline = common::expect_box_run(MAIN, "Main", &baseline_classpath, Some(jdk.as_path()));
    let krusty = common::expect_box_run(MAIN, "Main", &duplicated_classpath, Some(jdk.as_path()));
    let reference = reference_result(MAIN, &stdlibs[0], &coroutines);
    std::fs::remove_dir_all(directory).expect("remove copied-classpath fixture");

    assert_eq!(baseline, "OK");
    assert_eq!(krusty, "OK");
    assert_eq!(reference, "OK");
    assert_eq!(krusty, reference);
}
