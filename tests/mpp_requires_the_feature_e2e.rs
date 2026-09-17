//! `expect`/`actual` outside a multiplatform project is an error, not a no-op.
//!
//! krusty used to accept it and emit an artifact. That artifact could not link: a call to an
//! unmatched `expect fun helper(): Int` was written as `invokestatic plib/SameKt.helper:()I` into a
//! facade that declares no such method, so the program failed at its first call rather than at
//! compile time — the one outcome a compiler must never produce.
//!
//! Every fact below was measured against the reference compiler, including two that a guess would
//! have got wrong: the sentence does not vary with which of the two modifiers was written, and a
//! MEMBER `actual` is reported as well, at its own column.

use super::common;

/// kotlinc's exact sentence.
const MESSAGE: &str =
    "error: 'expect' and 'actual' declarations can be used only in multiplatform \
                       projects. Learn more about Kotlin Multiplatform: \
                       https://kotl.in/multiplatform-setup";

fn compile(source: &str, multiplatform: bool) -> (bool, String) {
    let dir = common::scratch_dir().expect("scratch dir");
    let src = dir.join("Main.kt");
    std::fs::write(&src, source).unwrap();
    let mut command = std::process::Command::new(common::krusty_binary());
    if multiplatform {
        command.arg("-XXLanguage:+MultiPlatformProjects");
    }
    let out = command
        .args(["-no-stdlib", "-no-jdk", "-cp"])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), report)
}

#[test]
fn an_expect_declaration_needs_the_multiplatform_feature() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         expect fun helper(): Int\n\
         \n\
         expect class Holder {\n\
         \x20   fun value(): Int\n\
         }\n",
        false,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    assert!(
        report.contains(&format!("Main.kt:3:1: {MESSAGE}")),
        "at the `expect` keyword, in the reference compiler's words:\n{report}"
    );
    assert!(
        report.contains(&format!("Main.kt:5:1: {MESSAGE}")),
        "once per declaration carrying it:\n{report}"
    );
}

/// An `actual` reports the same sentence — it names both modifiers either way — and a MEMBER
/// `actual` is reported at its own column, not at the class's.
#[test]
fn an_actual_declaration_needs_it_too_members_included() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         actual fun helper(): Int = 1\n\
         \n\
         class Wrapper {\n\
         \x20   actual fun member(): Int = 2\n\
         }\n",
        false,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    assert!(
        report.contains(&format!("Main.kt:3:1: {MESSAGE}")),
        "the top-level `actual`:\n{report}"
    );
    assert!(
        report.contains(&format!("Main.kt:6:5: {MESSAGE}")),
        "and the member one, at column 5:\n{report}"
    );
}

/// With the feature on, nothing changes: the same source compiles as before.
#[test]
fn the_feature_restores_the_previous_behaviour() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         expect fun helper(): Int\n\
         \n\
         actual fun helper(): Int = 1\n\
         \n\
         fun use(): Int = helper()\n",
        true,
    );
    assert!(ok, "a matched expect/actual pair still compiles:\n{report}");
    assert!(
        !report.contains("multiplatform"),
        "and says nothing about multiplatform projects:\n{report}"
    );
}

/// A file with neither modifier is untouched — the check must not cost an ordinary compile a
/// diagnostic, and `actual` as an ordinary IDENTIFIER is not a modifier.
#[test]
fn a_file_with_neither_modifier_is_untouched() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         fun use(actual: Int): Int = actual\n\
         \n\
         val expect: Int = 1\n",
        false,
    );
    assert!(ok, "an ordinary compile is unaffected:\n{report}");
    assert!(!report.contains("multiplatform"), "{report}");
}

/// An `expect` with no `actual`, with the feature on, in the reference compiler's own words and at
/// its position — the `expect` keyword, not the declaration it precedes.
///
/// The message names the module it looked in, so it tracks `-module-name`; the previous wording
/// ("expected declaration 'helper' has no actual declaration in this module", reported at the
/// declaration) said neither.
#[test]
fn an_unmatched_expect_names_the_module_it_looked_in() {
    const SOURCE: &str = "package plib\n\
                          \n\
                          expect fun helper(): Int\n\
                          \n\
                          expect class Holder {\n\
                          \x20   fun value(): Int\n\
                          }\n\
                          \n\
                          expect val prop: Int\n";

    let (ok, report) = compile(SOURCE, true);
    assert!(!ok, "the compile must fail:\n{report}");
    for (line, name) in [(3, "helper"), (5, "Holder"), (9, "prop")] {
        assert!(
            report.contains(&format!(
                "Main.kt:{line}:1: error: expected {name} has no actual declaration in module \
                 <main> for JVM"
            )),
            "{name} at the `expect` keyword:\n{report}"
        );
    }
}

/// And the module it names is the one `-module-name` gave it.
#[test]
fn the_module_a_diagnostic_names_is_the_declared_one() {
    let dir = common::scratch_dir().expect("scratch dir");
    let src = dir.join("Main.kt");
    std::fs::write(&src, "package plib\n\nexpect fun helper(): Int\n").unwrap();
    let out = std::process::Command::new(common::krusty_binary())
        .args([
            "-XXLanguage:+MultiPlatformProjects",
            "-module-name",
            "mylib",
            "-no-stdlib",
            "-no-jdk",
            "-cp",
        ])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(
        report
            .contains("error: expected helper has no actual declaration in module <mylib> for JVM"),
        "the declared module name, not the default:\n{report}"
    );
}
