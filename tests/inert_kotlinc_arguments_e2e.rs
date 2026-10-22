//! The kotlinc arguments krusty accepts without acting on them (`Disposition::Inert`) must leave
//! krusty's output identical to kotlinc's under the same argument. Each turns off a check kotlinc
//! makes and krusty does not. Two are checks on the classes a compilation reads: a library whose
//! metadata version is from the future, or whose classes come from a pre-release compiler, fails
//! under kotlinc unless the check is off. Those tests build such a library with kotlinc, then
//! compile a consumer with both compilers under the argument and compare the classes byte for
//! byte. The third is the reservation of the `kotlin` package for the standard library, which a
//! source declaring that package passes only with `-Xallow-kotlin-package`. The fourth is the
//! warning kotlinc reports for a `@Suppress` that names an error diagnostic. The fifth is the warning
//! kotlinc reports that expect/actual classes are Beta.

use std::path::{Path, PathBuf};

use super::common;

const LIBRARY: &str = "package lib\nfun hello(): String = \"O\"\nclass Box(val v: String)\n";
const CONSUMER: &str = "import lib.*\nfun use(): String = hello() + Box(\"K\").v\n";

fn classes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read compiler output") {
            let path = entry.expect("compiler output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let name = path
                    .strip_prefix(dir)
                    .expect("class below the output root")
                    .to_string_lossy()
                    .into_owned();
                found.push((name, std::fs::read(&path).expect("read class")));
            }
        }
    }
    found.sort();
    found
}

fn kotlinc(arguments: &[String]) -> (i32, String) {
    common::kotlinc_compile(arguments).expect("the reference kotlinc is available")
}

/// Compile the library with `library_arguments`, check kotlinc refuses the consumer without
/// `argument`, then compare both compilers' consumer classes with it.
fn assert_inert(argument: &str, library_arguments: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("Lib.kt"), LIBRARY).expect("write the library");
    std::fs::write(path("Use.kt"), CONSUMER).expect("write the consumer");

    let mut library = vec![text(&path("Lib.kt")), "-d".into(), text(&path("lib"))];
    library.extend(
        library_arguments
            .iter()
            .map(|argument| argument.to_string()),
    );
    library.push("-nowarn".into());
    let (code, stderr) = kotlinc(&library);
    assert_eq!(code, 0, "kotlinc library {library_arguments:?}: {stderr}");

    let consumer = |output: &str, with_argument: bool| {
        let mut arguments = vec![
            text(&path("Use.kt")),
            "-cp".into(),
            text(&path("lib")),
            "-d".into(),
            text(&path(output)),
        ];
        if with_argument {
            arguments.push(argument.to_string());
        }
        arguments
    };
    let (code, _) = kotlinc(&consumer("refused", false));
    assert_eq!(
        code, 1,
        "kotlinc must refuse the library without {argument}"
    );
    let (code, stderr) = kotlinc(&consumer("kotlinc", true));
    assert_eq!((code, stderr.as_str()), (0, ""), "kotlinc {argument}");

    let result = std::process::Command::new(common::krusty_binary())
        .args(consumer("krusty", true))
        .output()
        .expect("run krusty");
    assert_eq!(result.status.code(), Some(0), "krusty {argument}");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "",
        "krusty {argument}"
    );
    let expected = classes(&path("kotlinc"));
    assert!(!expected.is_empty(), "kotlinc emitted no classes");
    assert_eq!(classes(&path("krusty")), expected, "{argument}");
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn skipping_the_metadata_version_check_compiles_as_kotlinc_does() {
    assert_inert(
        "-Xskip-metadata-version-check",
        &["-Xmetadata-version=2.9.0"],
    );
}

#[test]
fn skipping_the_pre_release_check_compiles_as_kotlinc_does() {
    assert_inert("-Xskip-prerelease-check", &["-language-version", "2.5"]);
}

/// Compile one `source` file with both compilers under `argument`, compare their classes byte for
/// byte, and return kotlinc's exit code and diagnostics for the same source without `argument`,
/// with the source's path written as `<path>`.
fn assert_inert_for_source(argument: &str, file: &str, source: &str) -> (i32, String) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path(file), source).expect("write the source");
    let compile = |output: &str, with_argument: bool| {
        let mut arguments = vec![text(&path(file)), "-d".into(), text(&path(output))];
        if with_argument {
            arguments.push(argument.to_string());
        }
        arguments
    };
    let (code, without) = kotlinc(&compile("without", false));
    let (code_with, stderr) = kotlinc(&compile("kotlinc", true));
    assert_eq!((code_with, stderr.as_str()), (0, ""), "kotlinc {argument}");

    let result = std::process::Command::new(common::krusty_binary())
        .args(compile("krusty", true))
        .output()
        .expect("run krusty");
    assert_eq!(result.status.code(), Some(0), "krusty {argument}");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "",
        "krusty {argument}"
    );
    let expected = classes(&path("kotlinc"));
    assert!(!expected.is_empty(), "kotlinc emitted no classes");
    assert_eq!(classes(&path("krusty")), expected, "{argument}");
    // Normalize a source below the working directory whether kotlinc writes it absolutely or
    // relative to that directory.
    let source_path = path(file);
    let cwd = std::env::current_dir().expect("working directory");
    let written = source_path.strip_prefix(&cwd).unwrap_or(&source_path);
    // A recorded diagnostic can retain the absolute runner path. Replace the longer spelling first
    // so redacting the relative suffix cannot leave the runner prefix behind.
    let without = without
        .replace(&text(&source_path), "<path>")
        .replace(&text(written), "<path>");
    let _ = std::fs::remove_dir_all(work);
    (code, without)
}

#[test]
fn allowing_the_kotlin_package_compiles_as_kotlinc_does() {
    const ARGUMENT: &str = "-Xallow-kotlin-package";
    let source =
        "package kotlin.reserved\nclass Marker(val v: String)\nfun box(): String = Marker(\"OK\").v\n";
    let (code, _) = assert_inert_for_source(ARGUMENT, "Reserved.kt", source);
    assert_eq!(
        code, 1,
        "kotlinc must refuse the package without {ARGUMENT}"
    );
}

/// kotlinc warns about a `@Suppress` naming an error diagnostic unless this argument is given;
/// krusty never reports that warning, so under the argument both compilers are silent.
#[test]
fn not_warning_on_error_suppression_compiles_as_kotlinc_does() {
    const ARGUMENT: &str = "-Xdont-warn-on-error-suppression";
    let source = "package a\n@Suppress(\"INVISIBLE_REFERENCE\") fun f() = 1\n";
    let (code, stderr) = assert_inert_for_source(ARGUMENT, "Suppressing.kt", source);
    assert_eq!(
        (code, stderr.as_str()),
        (
            0,
            "<path>:2:11: warning: suppression of error 'INVISIBLE_REFERENCE' might compile and \
             work, but the compiler behavior is UNSPECIFIED and WILL NOT BE PRESERVED. Please \
             report your use case to the Kotlin issue tracker instead: https://kotl.in/issue\n\
             @Suppress(\"INVISIBLE_REFERENCE\") fun f() = 1\n          \
             ^^^^^^^^^^^^^^^^^^^^^\n"
        ),
        "kotlinc without {ARGUMENT}"
    );
}

/// kotlinc warns that expect/actual classes are Beta unless this argument is given; krusty never
/// reports that warning, so under the argument both compilers compile a refined fragment silently.
#[test]
fn allowing_expect_actual_classes_compiles_as_kotlinc_does() {
    const ARGUMENT: &str = "-Xexpect-actual-classes";
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(
        path("Common.kt"),
        "package p\nexpect class Box() {\n    fun v(): String\n}\n",
    )
    .expect("write the common source");
    std::fs::write(
        path("Platform.kt"),
        "package p\nactual class Box actual constructor() {\n    actual fun v() = \"OK\"\n}\n",
    )
    .expect("write the platform source");
    let compile = |output: &str, with_argument: bool| {
        let mut arguments = vec![
            text(&path("Common.kt")),
            text(&path("Platform.kt")),
            "-Xmulti-platform".to_string(),
            "-Xfragments=common,jvm".to_string(),
            format!("-Xfragment-sources=common:{}", text(&path("Common.kt"))),
            format!("-Xfragment-sources=jvm:{}", text(&path("Platform.kt"))),
            "-Xfragment-refines=jvm:common".to_string(),
            "-d".into(),
            text(&path(output)),
        ];
        if with_argument {
            arguments.push(ARGUMENT.to_string());
        }
        arguments
    };
    let (code, without) = kotlinc(&compile("without", false));
    // kotlinc writes a source below the working directory relative to it; redact both spellings.
    let cwd = std::env::current_dir().expect("working directory");
    let redact = |stderr: String, file: &str, placeholder: &str| {
        let source = path(file);
        let written = source.strip_prefix(&cwd).unwrap_or(&source);
        stderr
            .replace(&text(&source), placeholder)
            .replace(&text(written), placeholder)
    };
    let without = redact(
        redact(without, "Common.kt", "<common>"),
        "Platform.kt",
        "<platform>",
    );
    const BETA: &str = "warning: 'expect'/'actual' classes (including interfaces, objects, \
                        annotations, enums, and 'actual' typealiases) are in Beta. Consider using \
                        the '-Xexpect-actual-classes' flag to suppress this warning. Also see: \
                        https://youtrack.jetbrains.com/issue/KT-61573";
    assert_eq!(
        (code, without),
        (
            0,
            format!(
                "<common>:2:1: {BETA}\nexpect class Box() {{\n^^^^^^\n\
                 <platform>:2:1: {BETA}\nactual class Box actual constructor() {{\n^^^^^^\n"
            )
        ),
        "kotlinc without {ARGUMENT}"
    );
    let (code_with, stderr) = kotlinc(&compile("kotlinc", true));
    assert_eq!((code_with, stderr.as_str()), (0, ""), "kotlinc {ARGUMENT}");

    let result = std::process::Command::new(common::krusty_binary())
        .args(compile("krusty", true))
        .output()
        .expect("run krusty");
    assert_eq!(
        (
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).as_ref()
        ),
        (Some(0), ""),
        "krusty {ARGUMENT}"
    );
    let expected = classes(&path("kotlinc"));
    assert!(!expected.is_empty(), "kotlinc emitted no classes");
    assert_eq!(classes(&path("krusty")), expected, "{ARGUMENT}");
    let _ = std::fs::remove_dir_all(work);
}
