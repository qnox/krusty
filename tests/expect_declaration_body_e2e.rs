//! An `expect` declaration that carries an implementation is an error.
//!
//! A header declares; it does not implement. krusty used to misdiagnose every one of these shapes
//! as "no actual declaration", which sent a reader looking for a missing `actual` rather than at
//! the body they had written.
//!
//! Every position below is measured against the reference compiler, including three that a guess
//! would have got wrong: a property INITIALIZER is reported under the initializer expression rather
//! than at the property, an `init` block is reported at the `init` keyword rather than at its `{`,
//! and a secondary constructor with a body inside an `expect class` is not reported at all.

use super::common;

/// kotlinc's three sentences. None varies with the declaration's kind.
const BODY: &str = "error: expected declaration cannot have a body.";
const INITIALIZER: &str = "error: expected property cannot have an initializer.";
const DELEGATE: &str = "error: expected property cannot be delegated.";

fn compile_files(files: &[(&str, &str)], multiplatform: bool) -> (bool, String) {
    let dir = common::scratch_dir().expect("scratch dir");
    let mut command = std::process::Command::new(common::krusty_binary());
    if multiplatform {
        command.arg("-XXLanguage:+MultiPlatformProjects");
    }
    command
        .args(["-no-stdlib", "-no-jdk", "-cp"])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(&dir);
    for (name, source) in files {
        let path = dir.join(name);
        std::fs::write(&path, source).unwrap();
        command.arg(&path);
    }
    let out = command.output().expect("run krusty");
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    report.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), report)
}

fn compile(source: &str, multiplatform: bool) -> (bool, String) {
    compile_files(&[("Main.kt", source)], multiplatform)
}

/// Top-level shapes: an expression body, a block body, an initializer, and an accessor — each in
/// the reference compiler's words and at its own measured position.
#[test]
fn a_top_level_expect_declaration_may_not_implement_itself() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         expect fun exprBody(): Int = 1\n\
         \n\
         expect fun blockBody(): Int { return 2 }\n\
         \n\
         expect val initialized: Int = 3\n\
         \n\
         expect var withGetter: Int\n\
         \x20   get() = 4\n",
        true,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    // A function is reported at the `expect` KEYWORD, which introduces the whole header.
    assert!(report.contains(&format!("Main.kt:3:1: {BODY}")), "{report}");
    assert!(report.contains(&format!("Main.kt:5:1: {BODY}")), "{report}");
    // A property initializer is reported under the INITIALIZER, at column 31 here — not at the
    // property, and not at the `expect` keyword.
    assert!(
        report.contains(&format!("Main.kt:7:31: {INITIALIZER}")),
        "under the initializer expression:\n{report}"
    );
    // An accessor is a declaration of its own and is reported at its own header.
    assert!(
        report.contains(&format!("Main.kt:10:5: {BODY}")),
        "at the accessor:\n{report}"
    );
}

/// Members of an `expect` classifier are headers too — each reported at its own declaration, never
/// at the class.
#[test]
fn a_member_of_an_expect_classifier_may_not_implement_itself_either() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         expect class Holder {\n\
         \x20   val memberProp: Int = 1\n\
         \x20   val computed: Int\n\
         \x20       get() = 2\n\
         \x20   var settable: Int\n\
         \x20       set(v) { }\n\
         \x20   init { }\n\
         \x20   fun ok(): Int\n\
         }\n\
         \n\
         expect object Obj {\n\
         \x20   fun body(): Int = 3\n\
         }\n",
        true,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    assert!(
        report.contains(&format!("Main.kt:4:27: {INITIALIZER}")),
        "a member initializer, under the initializer:\n{report}"
    );
    assert!(report.contains(&format!("Main.kt:6:9: {BODY}")), "{report}");
    assert!(report.contains(&format!("Main.kt:8:9: {BODY}")), "{report}");
    // At the `init` KEYWORD (column 5), not at the `{` its block expression starts on.
    assert!(
        report.contains(&format!("Main.kt:9:5: {BODY}")),
        "an init block, at the keyword:\n{report}"
    );
    assert!(
        report.contains(&format!("Main.kt:14:5: {BODY}")),
        "an object member:\n{report}"
    );
    // The body-less member is silent, and so is the class itself.
    assert!(
        !report.contains("Main.kt:10:") && !report.contains("Main.kt:3:"),
        "a header member and the class are untouched:\n{report}"
    );
}

/// A NESTED classifier and a companion object inside an `expect class` are headers as well. The
/// parser hoists both out of their outer class, so this is the shape that would silently stop being
/// checked if that hoisting changed.
#[test]
fn a_nested_classifier_and_a_companion_are_headers_too() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         expect class Outer {\n\
         \x20   class Inner {\n\
         \x20       fun f(): Int = 1\n\
         \x20   }\n\
         \x20   companion object {\n\
         \x20       fun g(): Int = 2\n\
         \x20   }\n\
         }\n",
        true,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    assert!(report.contains(&format!("Main.kt:5:9: {BODY}")), "{report}");
    assert!(report.contains(&format!("Main.kt:8:9: {BODY}")), "{report}");
}

/// A delegate is an implementation too, with a sentence of its own, reported under the delegate
/// EXPRESSION rather than at the `by` or at the property.
#[test]
fn an_expect_property_may_not_be_delegated() {
    let (ok, report) = compile(
        "package plib\n\
         \n\
         expect val delegated: Int by lazy { 1 }\n\
         \n\
         expect class C {\n\
         \x20   val member: Int by lazy { 2 }\n\
         }\n",
        true,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    assert!(
        report.contains(&format!("Main.kt:3:30: {DELEGATE}")),
        "under the delegate expression:\n{report}"
    );
    assert!(
        report.contains(&format!("Main.kt:6:24: {DELEGATE}")),
        "a member delegate too:\n{report}"
    );
}

/// A secondary constructor with a body inside an `expect class` is NOT reported — measured, and
/// the opposite of what the neighbouring shapes suggest.
#[test]
fn a_secondary_constructor_body_is_not_this_diagnostic() {
    let (_, report) = compile(
        "package plib\n\
         \n\
         expect class A {\n\
         \x20   constructor(x: Int) { }\n\
         }\n",
        true,
    );
    assert!(
        !report.contains(BODY),
        "the reference compiler says nothing here:\n{report}"
    );
}

/// Once any `expect` body exists the reference compiler never reaches actualization, so the
/// unmatched-expect report is suppressed for the WHOLE compilation — not just for the file that
/// carried the body. Measured with exactly this two-file shape.
#[test]
fn a_body_error_suppresses_the_unmatched_expect_report_everywhere() {
    let (ok, report) = compile_files(
        &[
            ("A.kt", "package plib\nexpect fun withBody(): Int = 1\n"),
            ("B.kt", "package plib\nexpect fun clean(): Int\n"),
        ],
        true,
    );
    assert!(!ok, "the compile must fail:\n{report}");
    assert!(report.contains(&format!("A.kt:2:1: {BODY}")), "{report}");
    assert!(
        !report.contains("has no actual declaration"),
        "the clean unmatched expect in the other file stays silent:\n{report}"
    );
}

/// The check is syntactic, so it does not depend on the multiplatform feature: a file without it
/// gets BOTH sentences, the feature-gate one first, exactly as the reference compiler orders them.
#[test]
fn the_body_is_rejected_with_or_without_the_multiplatform_feature() {
    let (ok, report) = compile("package plib\n\nexpect fun exprBody(): Int = 1\n", false);
    assert!(!ok, "the compile must fail:\n{report}");
    let gate = report
        .find("can be used only in multiplatform projects")
        .expect("the feature-gate diagnostic");
    let body = report.find(BODY).expect("the body diagnostic");
    assert!(gate < body, "the feature gate is reported first:\n{report}");
    assert!(report.contains(&format!("Main.kt:3:1: {BODY}")), "{report}");
}

/// And a body-less `expect` is untouched: the check must not cost an ordinary header a diagnostic.
#[test]
fn a_body_less_expect_is_untouched() {
    let (_, report) = compile(
        "package plib\n\
         \n\
         expect fun helper(): Int\n\
         \n\
         expect val prop: Int\n\
         \n\
         expect class Holder {\n\
         \x20   fun value(): Int\n\
         }\n",
        true,
    );
    assert!(
        !report.contains(BODY) && !report.contains(INITIALIZER),
        "a header declares nothing to reject:\n{report}"
    );
    // It is still unmatched, and that report is unaffected.
    assert!(
        report.contains("has no actual declaration"),
        "the unmatched-expect report still runs:\n{report}"
    );
}
