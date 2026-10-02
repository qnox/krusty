//! A default lambda written on an `expect` declaration is spliced into the actual call, and its
//! inline-depth marker is spelled after the class the actual declaration compiles to. The expect
//! header is gone before that default is checked, so the naming walk's parser owner has to be read
//! as the actual classifier.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::common;

const COMMON: &str = r#"
// LANGUAGE: +MultiPlatformProjects

expect inline fun topLevel(a: String, b: Int = 0, c: () -> Int = { 0 }): Int

expect class Foo() {
    inline fun member(a: String, b: Int = 0, c: () -> Int = { 0 }): Int
}
"#;

const PLATFORM: &str = r#"
actual inline fun topLevel(a: String, b: Int, c: () -> Int): Int = a.length + b + c()

actual class Foo actual constructor() {
    actual inline fun member(a: String, b: Int, c: () -> Int): Int = a.length + b + c()
}

fun box(): String {
    if (topLevel("OK") != 2) return "fail top"
    if (topLevel("OK", 1) != 3) return "fail top b"
    if (topLevel("OK", 1, { 4 }) != 7) return "fail top c"
    val foo = Foo()
    if (foo.member("OK") != 2) return "fail member"
    if (foo.member("OK", 1) != 3) return "fail member b"
    if (foo.member("OK", 1, { 4 }) != 7) return "fail member c"
    return "OK"
}
"#;

#[test]
fn expect_default_lambda_markers_match_kotlinc() {
    let sources = [("common.kt", COMMON), ("platform.kt", PLATFORM)];
    common::expect_box_ok_files_with_stdlib(&sources, "expect default lambda");

    let krusty = marker_names_of_class(&compile_krusty(&sources), "PlatformKt");
    let reference = kotlinc_marker_names();
    assert_eq!(krusty, reference);
}

fn compile_krusty(sources: &[(&str, &str)]) -> PathBuf {
    let classes = common::compile_in_process_files(
        sources,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles the expect default lambda");
    let dir = std::env::temp_dir().join(format!(
        "krusty_expect_default_lambda_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    for (name, bytes) in classes {
        let path = dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("class dir");
        }
        fs::write(path, bytes).expect("class bytes");
    }
    dir
}

fn kotlinc_marker_names() -> BTreeSet<String> {
    let dir = std::env::temp_dir().join(format!(
        "kotlinc_expect_default_lambda_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("kotlinc scratch dir");
    let common = dir.join("common.kt");
    let platform = dir.join("platform.kt");
    let out = dir.join("out");
    fs::write(&common, COMMON).expect("common source");
    fs::write(&platform, PLATFORM).expect("platform source");
    let args = vec![
        "-Xmulti-platform".to_string(),
        "-Xexpect-actual-classes".to_string(),
        format!("-Xcommon-sources={}", common.display()),
        "-d".to_string(),
        out.display().to_string(),
        "-cp".to_string(),
        common::stdlib_jar().display().to_string(),
        common.display().to_string(),
        platform.display().to_string(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(
        code, 0,
        "kotlinc rejected the expect default lambda: {stderr}"
    );
    marker_names_of_class(&out, "PlatformKt")
}

fn marker_names_of_class(dir: &Path, class: &str) -> BTreeSet<String> {
    let classpath = dir.to_string_lossy().into_owned();
    let text = common::javap(&["-p", "-l", "-cp", &classpath, class])
        .expect("javap of the platform facade");
    text.split_whitespace()
        .filter(|word| word.starts_with("$i$"))
        .map(str::to_owned)
        .collect()
}
