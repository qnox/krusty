//! The language features a command line selects, compared with kotlinc: what it reports about
//! `-XXLanguage` values and `@Enables` arguments, its exit status, and every file it writes,
//! including the pre-release flag of `@kotlin.Metadata` when the settings are pre-release.

use std::path::{Path, PathBuf};

use super::common;

const SOURCE: &str = "package p\nclass Box(val v: String)\nfun box(): String = Box(\"OK\").v\n";

/// Every file below `dir`, by its path relative to `dir`, with its bytes. A missing directory is an
/// empty output.
fn output_tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("compiler output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path
                    .strip_prefix(dir)
                    .expect("a file below the output root")
                    .to_string_lossy()
                    .into_owned();
                found.push((name, std::fs::read(&path).expect("read an output file")));
            }
        }
    }
    found.sort();
    found
}

/// Compile [`SOURCE`] with both compilers under `arguments`; both must exit alike, report the same
/// words, and write the same output tree byte for byte, which is empty when they fail.
fn assert_like_kotlinc(arguments: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("Box.kt"), SOURCE).expect("write the source");
    let command_line = |output: &str| {
        let mut command_line = vec![text(&path("Box.kt")), "-d".into(), text(&path(output))];
        command_line.extend(arguments.iter().map(|argument| argument.to_string()));
        command_line
    };
    let (kotlinc_code, kotlinc_stderr) = common::kotlinc_compile(&command_line("kotlinc"))
        .expect("the reference kotlinc is available");
    let krusty = std::process::Command::new(common::krusty_binary())
        .args(command_line("krusty"))
        .output()
        .expect("run krusty");
    assert_eq!(
        (
            krusty.status.code(),
            String::from_utf8_lossy(&krusty.stderr).as_ref()
        ),
        (Some(kotlinc_code), kotlinc_stderr.as_str()),
        "{arguments:?}"
    );
    let expected = output_tree(&path("kotlinc"));
    assert_eq!(
        expected.is_empty(),
        kotlinc_code != 0,
        "kotlinc writes output exactly when it succeeds"
    );
    assert_eq!(output_tree(&path("krusty")), expected, "{arguments:?}");
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn an_unknown_feature_is_reported_and_skipped() {
    assert_like_kotlinc(&["-XXLanguage:+NoSuchFeature"]);
}

#[test]
fn a_setting_without_a_sign_or_a_name_is_reported_and_skipped() {
    assert_like_kotlinc(&["-XXLanguage:WhenGuards"]);
    assert_like_kotlinc(&["-XXLanguage:+"]);
}

/// kotlinc reads one feature per argument: a comma is part of the name.
#[test]
fn a_comma_does_not_separate_settings() {
    assert_like_kotlinc(&["-XXLanguage:+WhenGuards,-ContextParameters"]);
}

#[test]
fn a_manual_setting_is_listed_in_the_unsafe_arguments_notice() {
    assert_like_kotlinc(&["-XXLanguage:+NameBasedDestructuring"]);
    assert_like_kotlinc(&["-XXLanguage:-WhenGuards", "-XXLanguage:+WhenGuards"]);
}

#[test]
fn a_test_only_feature_fails_the_compilation() {
    assert_like_kotlinc(&["-XXLanguage:+ImplicitSignedToUnsignedIntegerConversion"]);
}

#[test]
fn a_feature_from_an_unsupported_version_cannot_be_disabled() {
    assert_like_kotlinc(&["-XXLanguage:-TypeAliases"]);
    assert_like_kotlinc(&[
        "-language-version",
        "2.0",
        "-XXLanguage:-TypeAliases",
        "-XXLanguage:+FullValueClasses",
    ]);
}

#[test]
fn an_unreleased_feature_marks_the_classes_pre_release() {
    assert_like_kotlinc(&["-XXLanguage:+FullValueClasses"]);
}

#[test]
fn an_experimental_language_version_marks_the_classes_pre_release() {
    assert_like_kotlinc(&["-language-version", "2.5"]);
}

#[test]
fn an_enables_argument_that_changes_no_default_is_redundant() {
    assert_like_kotlinc(&["-Xmulti-dollar-interpolation"]);
    assert_like_kotlinc(&[
        "-XXLanguage:-MultiDollarInterpolation",
        "-Xmulti-dollar-interpolation",
    ]);
    assert_like_kotlinc(&[
        "-language-version",
        "2.5",
        "-Xname-based-destructuring=only-syntax",
    ]);
}
