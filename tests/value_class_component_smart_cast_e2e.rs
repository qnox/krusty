//! `AllowSmartCastsOnValueClassUnderlyingProperties` (2.5.0-Beta2): a component of a value class
//! declared in another module is a stable smart-cast subject, for the single underlying property
//! of an inline value class and for each field of a multi-field one. Any other property of another
//! module stays unstable. Both compilers read a library krusty built, so what is compared is how
//! each reads the library's metadata.

use std::path::{Path, PathBuf};

use super::common;

const RELEASE: &str = "2.5.0-Beta2";

const LIBRARY: &str = "\
package lib
value class MultiField(val first: String?, val second: Int?)
@JvmInline value class SingleField(val only: String?)
class Plain(val name: String?)
";

const CONSUMER: &str = "\
import lib.MultiField
import lib.Plain
import lib.SingleField

fun first(v: MultiField): String = if (v.first != null) v.first else \"none\"
fun second(v: MultiField): Int = if (v.second != null) v.second else 0
fun only(v: SingleField): String = if (v.only != null) v.only else \"none\"
fun name(p: Plain): String = if (p.name != null) p.name else \"none\"
";

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn krusty(args: &[String]) -> (i32, String) {
    let mut all = vec![format!("-Xkotlin-reference-version={RELEASE}")];
    all.extend_from_slice(args);
    let result = std::process::Command::new(common::krusty_binary())
        .args(all)
        .output()
        .expect("run krusty");
    (
        result.status.code().unwrap_or(1),
        String::from_utf8_lossy(&result.stderr).into_owned(),
    )
}

fn build_library(work: &Path) -> PathBuf {
    let source = work.join("Library.kt");
    std::fs::write(&source, LIBRARY).expect("write the library");
    let output = work.join("library");
    let (code, stderr) = krusty(&[
        text(&source),
        "-d".into(),
        text(&output),
        "-language-version".into(),
        "2.5".into(),
        "-XXLanguage:+FullValueClasses".into(),
    ]);
    assert_eq!(code, 0, "krusty builds the library: {stderr}");
    output
}

fn assert_like_kotlinc(work: &Path, library: &Path, label: &str, arguments: &[&str]) {
    let consumer = work.join("Consumer.kt");
    std::fs::write(&consumer, CONSUMER).expect("write the consumer");
    let compile = |reference: bool| -> (i32, String) {
        let output = work.join(format!("{label}-{reference}"));
        let mut args = vec![
            text(&consumer),
            "-cp".into(),
            text(library),
            "-d".into(),
            text(&output),
            "-language-version".into(),
            "2.5".into(),
        ];
        args.extend(arguments.iter().map(|argument| argument.to_string()));
        if reference {
            common::kotlinc_compile_for_release(RELEASE, &args).expect("kotlinc 2.5.0-Beta2")
        } else {
            krusty(&args)
        }
    };
    let (kotlinc_code, kotlinc_stderr) = compile(true);
    let (krusty_code, krusty_stderr) = compile(false);
    let located = |stderr: &str| common::compiler_errors(stderr);
    assert_eq!(
        (krusty_code, located(&krusty_stderr)),
        (kotlinc_code, located(&kotlinc_stderr)),
        "{label}\nkrusty: {krusty_stderr}\nkotlinc: {kotlinc_stderr}"
    );
}

#[test]
fn value_class_components_of_another_module_follow_the_language_feature() {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let library = build_library(&work);
    // Without the feature every property of the other module is unstable.
    assert_like_kotlinc(
        &work,
        &library,
        "disabled",
        &["-XXLanguage:+FullValueClasses"],
    );
    // With it the value class components are stable and `Plain.name` still is not.
    assert_like_kotlinc(
        &work,
        &library,
        "enabled",
        &[
            "-XXLanguage:+FullValueClasses",
            "-XXLanguage:+AllowSmartCastsOnValueClassUnderlyingProperties",
        ],
    );
    let _ = std::fs::remove_dir_all(&work);
}
