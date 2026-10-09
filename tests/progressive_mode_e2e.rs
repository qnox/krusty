//! `-progressive` turns on the release's progressive language features (its `LanguageFeature`
//! entries with `actuallyEnabledInProgressiveMode`), and warns when the language version is older
//! than the latest stable one. Each case compiles one file with kotlinc and krusty under the same
//! arguments and compares the exit code, the complete stderr, and the class bytes.

use super::common;

const SOURCE: &str = "fun box(): String = listOf(\"O\", \"K\").joinToString(\"\")\n";

fn assert_same_as_kotlinc(extra: &[&str]) {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let source = work.join("Box.kt");
    std::fs::write(&source, SOURCE).expect("write the source");
    let arguments = |output: &str| -> Vec<String> {
        let mut arguments = vec![
            source.to_string_lossy().into_owned(),
            "-d".into(),
            work.join(output).to_string_lossy().into_owned(),
        ];
        arguments.extend(extra.iter().map(|argument| argument.to_string()));
        arguments
    };
    let (code, kotlinc_stderr) =
        common::kotlinc_compile(&arguments("kotlinc")).expect("the reference kotlinc is available");
    assert_eq!(code, 0, "kotlinc {extra:?}: {kotlinc_stderr}");

    let krusty = std::process::Command::new(common::krusty_binary())
        .args(arguments("krusty"))
        .output()
        .expect("run krusty");
    assert_eq!(krusty.status.code(), Some(0), "krusty {extra:?}");
    assert_eq!(
        String::from_utf8_lossy(&krusty.stderr),
        kotlinc_stderr,
        "krusty {extra:?}"
    );
    let class = |output: &str| std::fs::read(work.join(output).join("BoxKt.class"));
    assert_eq!(
        class("krusty").expect("krusty wrote BoxKt.class"),
        class("kotlinc").expect("kotlinc wrote BoxKt.class"),
        "{extra:?}"
    );
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn progressive_mode_compiles_as_kotlinc_does() {
    assert_same_as_kotlinc(&["-progressive"]);
}

#[test]
fn progressive_mode_on_an_older_language_version_warns_as_kotlinc_does() {
    assert_same_as_kotlinc(&[
        "-progressive",
        "-language-version",
        "2.3",
        "-api-version",
        "2.3",
    ]);
}

#[test]
fn suppressed_version_warnings_silence_the_progressive_warning_as_in_kotlinc() {
    assert_same_as_kotlinc(&[
        "-progressive",
        "-language-version",
        "2.3",
        "-Xsuppress-version-warnings",
    ]);
}
