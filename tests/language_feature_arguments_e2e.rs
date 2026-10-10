//! The language features a command line selects, compared with kotlinc: what it reports about
//! `-XXLanguage` values and `@Enables` arguments, its exit status, and every file it writes,
//! including the pre-release flag of `@kotlin.Metadata` when the settings are pre-release.

use std::path::{Path, PathBuf};

use super::common;

const SOURCE: &str = "package p\nclass Box(val v: String)\nfun box(): String = Box(\"OK\").v\n";

/// Compile [`SOURCE`] with both compilers under `arguments`; both must exit alike, report the same
/// words, and write the same output tree byte for byte, which is empty when they fail.
fn assert_like_kotlinc(arguments: &[&str]) {
    assert_source_like_kotlinc(SOURCE, arguments);
}

/// [`assert_like_kotlinc`] for `source`.
fn assert_source_like_kotlinc(source: &str, arguments: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let path = |name: &str| -> PathBuf { work.join(name) };
    let text = |path: &Path| path.to_string_lossy().into_owned();
    std::fs::write(path("Box.kt"), source).expect("write the source");
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
    let expected = common::output_tree(&path("kotlinc"));
    assert_eq!(
        expected.is_empty(),
        kotlinc_code != 0,
        "kotlinc writes output exactly when it succeeds"
    );
    assert_eq!(
        common::output_tree(&path("krusty")),
        expected,
        "{arguments:?}"
    );
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

/// The klib inliner features select lowerings kotlinc runs only before serializing a klib, so a
/// JVM compilation is the same with either state. kotlinc still refuses the cross-module inliner
/// unless the intra-module one is explicitly enabled too, and the cross-module inliner forces
/// pre-release binaries.
#[test]
fn klib_inliner_features_are_checked_for_consistency() {
    assert_like_kotlinc(&["-XXLanguage:+IrCrossModuleInlinerBeforeKlibSerialization"]);
    assert_like_kotlinc(&[
        "-XXLanguage:-IrIntraModuleInlinerBeforeKlibSerialization",
        "-XXLanguage:+IrCrossModuleInlinerBeforeKlibSerialization",
    ]);
    assert_like_kotlinc(&[
        "-XXLanguage:+IrCrossModuleInlinerBeforeKlibSerialization",
        "-XXLanguage:-TypeAliases",
    ]);
}

const INLINE_SOURCE: &str = r#"package p

inline fun <reified T> isOf(value: Any): Boolean = value is T

fun box(): String = if (isOf<String>("x")) "OK" else "fail"
"#;

#[test]
fn klib_inliner_features_do_not_change_jvm_output() {
    for arguments in [
        &[
            "-XXLanguage:+IrIntraModuleInlinerBeforeKlibSerialization",
            "-XXLanguage:+IrCrossModuleInlinerBeforeKlibSerialization",
        ][..],
        &["-XXLanguage:-IrIntraModuleInlinerBeforeKlibSerialization"],
        &[
            "-XXLanguage:-IrIntraModuleInlinerBeforeKlibSerialization",
            "-XXLanguage:-IrCrossModuleInlinerBeforeKlibSerialization",
        ],
        &[],
    ] {
        assert_source_like_kotlinc(INLINE_SOURCE, arguments);
    }
}
