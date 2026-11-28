//! `-Xreturn-value-checker` and the ignorability annotations it admits, compared with kotlinc:
//! its exit status, every word it reports, and every file it writes.
//!
//! The annotations `@MustUseReturnValues` and `@IgnorableReturnValue` are rejected while the
//! checker is disabled and the language version has not made it stable. The mode is `default`
//! unless the command line names one, and `default` means `check` from the release that made the
//! checker stable and `disable` before it. A release before 2.5 has no `default` mode and refuses
//! the value, which the same comparisons pin on that release.

use std::path::{Path, PathBuf};

use super::common;
use super::compiler_plugin_fixture;

/// The annotations on a file, a class and a function. Nothing here is an unused return value, so
/// the only diagnostics are about the annotations themselves.
const ANNOTATED: &str = "\
@file:MustUseReturnValues
package p

@MustUseReturnValues
class Marked {
    @IgnorableReturnValue
    fun quiet(): String = \"\"
}

@kotlin.MustUseReturnValues
object Qualified

@kotlin.IgnorableReturnValue
fun ignorable(): String = \"\"
";

/// The release under test, whose kotlinc each case is compared with. Where that release has not
/// made the checker stable, the cases that expect a stable checker pin that it is still disabled.
fn release() -> String {
    krusty::kotlin_version::environment_release().to_string()
}

/// The diagnostics a compiler printed, without kotlinc's source excerpts and with the scratch
/// directory the source sits in removed.
fn ledger(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("Source.kt") && !line.starts_with("krusty:"))
        .map(|line| match line.find("Source.kt") {
            Some(at) => line[at..].to_string(),
            None => line.to_string(),
        })
        .collect()
}

/// How much of the output tree two compilers must share.
#[derive(Clone, Copy, PartialEq)]
enum Output {
    /// The same class files by name.
    Classes,
    /// The same class files with the same bytes.
    Bytes,
}

/// Compile `source` with both compilers under `arguments`; both must exit alike, report the same
/// words, and write the same class files, which are none when they fail.
fn assert_like_kotlinc(source: &str, arguments: &[&str]) {
    assert_compiles_like_kotlinc(source, arguments, Output::Classes);
}

/// [`assert_like_kotlinc`] with every class file byte for byte the same.
fn assert_identical_to_kotlinc(source: &str, arguments: &[&str]) {
    assert_compiles_like_kotlinc(source, arguments, Output::Bytes);
}

fn assert_compiles_like_kotlinc(source: &str, arguments: &[&str], output: Output) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("Source.kt"), source).expect("write the source");
    let command_line = |output: &str| {
        let mut command_line = vec![text(&path("Source.kt")), "-d".into(), text(&path(output))];
        command_line.extend(arguments.iter().map(|argument| argument.to_string()));
        command_line
    };
    let (kotlinc_code, kotlinc_stderr) =
        common::kotlinc_server::kotlinc_compile_for_release(&release(), &command_line("kotlinc"))
            .expect("the reference kotlinc is available");
    let krusty = std::process::Command::new(common::krusty_binary())
        .arg(format!("-Xkotlin-reference-version={}", release()))
        .args(command_line("krusty"))
        .output()
        .expect("run krusty");
    assert_eq!(
        (
            krusty.status.code(),
            ledger(&String::from_utf8_lossy(&krusty.stderr))
        ),
        (Some(kotlinc_code), ledger(&kotlinc_stderr)),
        "{arguments:?}"
    );
    if kotlinc_code == 0 {
        match output {
            Output::Bytes => {
                compiler_plugin_fixture::assert_same_classes(&path("kotlinc"), &path("krusty"));
            }
            Output::Classes => assert_eq!(
                class_files(&path("krusty")),
                class_files(&path("kotlinc")),
                "{arguments:?}"
            ),
        }
    }
    let _ = std::fs::remove_dir_all(work);
}

/// The class files under `directory`, relative to it, in name order.
fn class_files(directory: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next).expect("read the output directory") {
            let entry = entry.expect("read an output entry").path();
            if entry.is_dir() {
                pending.push(entry);
            } else if let Ok(relative) = entry.strip_prefix(directory) {
                names.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    names
}

#[test]
fn the_annotations_are_rejected_when_the_checker_is_disabled_by_default() {
    assert_like_kotlinc(ANNOTATED, &["-language-version=2.4"]);
}

#[test]
fn the_annotations_are_rejected_under_an_explicit_disable() {
    assert_like_kotlinc(
        ANNOTATED,
        &["-language-version=2.4", "-Xreturn-value-checker=disable"],
    );
}

#[test]
fn a_mode_that_enables_the_checker_admits_the_annotations() {
    for mode in ["check", "full"] {
        assert_like_kotlinc(
            ANNOTATED,
            &[
                "-language-version=2.4",
                &format!("-Xreturn-value-checker={mode}"),
            ],
        );
    }
}

#[test]
fn the_default_mode_follows_the_language_version() {
    assert_like_kotlinc(
        ANNOTATED,
        &["-language-version=2.4", "-Xreturn-value-checker=default"],
    );
    assert_like_kotlinc(ANNOTATED, &["-Xreturn-value-checker=default"]);
}

/// From the release that made the checker stable the annotations are accepted whatever the mode.
#[test]
fn the_annotations_are_admitted_once_the_checker_is_stable() {
    assert_like_kotlinc(ANNOTATED, &[]);
    assert_like_kotlinc(ANNOTATED, &["-Xreturn-value-checker=disable"]);
}

const UNNAMED_LOCAL: &str = "fun test(text: String) {\n    val _ = text\n}\n";

/// A checking mode enables the feature a local `val _` needs; before 2.5 nothing else does.
#[test]
fn a_checking_mode_enables_unnamed_local_variables() {
    assert_identical_to_kotlinc(UNNAMED_LOCAL, &["-language-version=2.4"]);
    assert_identical_to_kotlinc(
        UNNAMED_LOCAL,
        &["-language-version=2.4", "-Xreturn-value-checker=check"],
    );
    assert_identical_to_kotlinc(
        UNNAMED_LOCAL,
        &["-language-version=2.4", "-Xreturn-value-checker=disable"],
    );
    assert_identical_to_kotlinc(UNNAMED_LOCAL, &[]);
}
