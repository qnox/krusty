//! The kotlinc arguments krusty accepts without acting on them (`Disposition::Inert`) must leave
//! krusty's output identical to kotlinc's under the same argument. Each turns off a check kotlinc
//! makes and krusty does not. Two are checks on the classes a compilation reads: a library whose
//! metadata version is from the future, or whose classes come from a pre-release compiler, fails
//! under kotlinc unless the check is off. Those tests build such a library with kotlinc, then
//! compile a consumer with both compilers under the argument and compare the classes byte for
//! byte. The third is the reservation of the `kotlin` package for the standard library, which a
//! source declaring that package passes only with `-Xallow-kotlin-package`.

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

#[test]
fn allowing_the_kotlin_package_compiles_as_kotlinc_does() {
    const ARGUMENT: &str = "-Xallow-kotlin-package";
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(
        path("Reserved.kt"),
        "package kotlin.reserved\nclass Marker(val v: String)\nfun box(): String = Marker(\"OK\").v\n",
    )
    .expect("write the source");
    let compile = |output: &str, with_argument: bool| {
        let mut arguments = vec![text(&path("Reserved.kt")), "-d".into(), text(&path(output))];
        if with_argument {
            arguments.push(ARGUMENT.to_string());
        }
        arguments
    };
    let (code, _) = kotlinc(&compile("refused", false));
    assert_eq!(
        code, 1,
        "kotlinc must refuse the package without {ARGUMENT}"
    );
    let (code, stderr) = kotlinc(&compile("kotlinc", true));
    assert_eq!((code, stderr.as_str()), (0, ""), "kotlinc {ARGUMENT}");

    let result = std::process::Command::new(common::krusty_binary())
        .args(compile("krusty", true))
        .output()
        .expect("run krusty");
    assert_eq!(result.status.code(), Some(0), "krusty {ARGUMENT}");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "",
        "krusty {ARGUMENT}"
    );
    let expected = classes(&path("kotlinc"));
    assert!(!expected.is_empty(), "kotlinc emitted no classes");
    assert_eq!(classes(&path("krusty")), expected, "{ARGUMENT}");
    let _ = std::fs::remove_dir_all(work);
}
