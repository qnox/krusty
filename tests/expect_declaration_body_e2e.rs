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
/// The default module name both compilers use when none is given, which the unmatched-expect
/// sentence names.
const MODULE: &str = "main";

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

/// One compiler's complete ordered diagnostic ledger, as `<file>:<line>:<column>: <severity>: <msg>`.
///
/// Both compilers print a source echo and a caret run under each header; those are a rendering of
/// the position the header already states exactly, so the ledger keeps every header and drops only
/// that rendering. Nothing is filtered by content: an entry this change did not intend is a failure
/// here, which is the point of comparing the whole sequence rather than probing it for substrings.
fn ledger(report: &str, sources: &[(&str, &str)]) -> Vec<String> {
    let echoes: Vec<&str> = sources
        .iter()
        .flat_map(|(_, source)| source.lines())
        .map(str::trim_end)
        .collect();
    report
        .lines()
        .filter_map(|line| {
            let (path, rest) = line.split_once(':')?;
            let name = path.rsplit('/').next()?;
            if !sources.iter().any(|(source, _)| *source == name) {
                return None;
            }
            let (line_no, rest) = rest.split_once(':')?;
            let (column, message) = rest.split_once(": ")?;
            line_no.parse::<u32>().ok()?;
            column.parse::<u32>().ok()?;
            Some(format!("{name}:{line_no}:{column}: {message}"))
        })
        .chain(std::iter::empty())
        .filter(|entry| !echoes.contains(&entry.as_str()))
        .collect()
}

/// The reference compiler's ledger for the same sources, so every expectation below is measured
/// rather than pinned. A missing reference compiler fails the test; it does not pass it.
fn kotlinc_ledger(sources: &[(&str, &str)]) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let out = dir.join("kotlinc-out");
    let mut args = vec![
        "-nowarn".to_string(),
        "-Xmulti-platform".to_string(),
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    for (name, source) in sources {
        let path = dir.join(name);
        std::fs::write(&path, source).expect("write source");
        args.push(path.to_string_lossy().into_owned());
    }
    let (code, stderr) =
        common::kotlinc_compile(&args).expect("reference kotlinc unavailable under the harness");
    assert_ne!(code, 0, "kotlinc must reject these sources:\n{stderr}");
    ledger(&stderr, sources)
}

/// Top-level shapes: an expression body, a block body, an initializer, and an accessor — each in
/// the reference compiler's words and at its own measured position.
#[test]
fn a_top_level_expect_declaration_may_not_implement_itself() {
    const SOURCE: &str = "package plib\n\
                          \n\
                          expect fun exprBody(): Int = 1\n\
                          \n\
                          expect fun blockBody(): Int { return 2 }\n\
                          \n\
                          expect val initialized: Int = 3\n\
                          \n\
                          expect var withGetter: Int\n\
                          \x20   get() = 4\n";
    let (ok, report) = compile(SOURCE, true);
    assert!(!ok, "the compile must fail:\n{report}");
    // A function is reported at the `expect` KEYWORD, which introduces the whole header; a property
    // initializer under the INITIALIZER, not at the property and not at the keyword; an accessor at
    // its own header. Complete and ordered, so an entry neither compiler writes fails here.
    let expected = vec![
        format!("Main.kt:3:1: {BODY}"),
        format!("Main.kt:5:1: {BODY}"),
        format!("Main.kt:7:31: {INITIALIZER}"),
        format!("Main.kt:10:5: {BODY}"),
    ];
    let sources = [("Main.kt", SOURCE)];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
    );
}

/// Members of an `expect` classifier are headers too — each reported at its own declaration, never
/// at the class.
#[test]
fn a_member_of_an_expect_classifier_may_not_implement_itself_either() {
    const SOURCE: &str = "package plib\n\
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
                          }\n";
    let (ok, report) = compile(SOURCE, true);
    assert!(!ok, "the compile must fail:\n{report}");
    // A member initializer under the initializer; an `init` block at the KEYWORD (column 5), not at
    // the `{` its block expression starts on. The body-less member at line 10 and the classifiers
    // themselves are absent, which the complete ledger states rather than probing for.
    let expected = vec![
        format!("Main.kt:4:27: {INITIALIZER}"),
        format!("Main.kt:6:9: {BODY}"),
        format!("Main.kt:8:9: {BODY}"),
        format!("Main.kt:9:5: {BODY}"),
        format!("Main.kt:14:5: {BODY}"),
    ];
    let sources = [("Main.kt", SOURCE)];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
    );
}

/// A NESTED classifier and a companion object inside an `expect class` are headers as well. The
/// parser hoists both out of their outer class, so this is the shape that would silently stop being
/// checked if that hoisting changed.
#[test]
fn a_nested_classifier_and_a_companion_are_headers_too() {
    const SOURCE: &str = "package plib\n\
                          \n\
                          expect class Outer {\n\
                          \x20   class Inner {\n\
                          \x20       fun f(): Int = 1\n\
                          \x20   }\n\
                          \x20   companion object {\n\
                          \x20       fun g(): Int = 2\n\
                          \x20   }\n\
                          }\n";
    let (ok, report) = compile(SOURCE, true);
    assert!(!ok, "the compile must fail:\n{report}");
    let expected = vec![
        format!("Main.kt:5:9: {BODY}"),
        format!("Main.kt:8:9: {BODY}"),
    ];
    let sources = [("Main.kt", SOURCE)];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
    );
}

/// A delegate is an implementation too, with a sentence of its own, reported under the delegate
/// EXPRESSION rather than at the `by` or at the property.
///
/// The delegate is a fixture-owned `Cache`, not `lazy`: `by lazy { … }` is a stdlib shape whose
/// recognition could carry this on its own, and the rule under test is about the `by` clause being
/// an implementation at all, whatever supplies it.
#[test]
fn an_expect_property_may_not_be_delegated() {
    const SOURCE: &str = "package plib\n\
                          \n\
                          import kotlin.reflect.KProperty\n\
                          \n\
                          class Cache {\n\
                          \x20   operator fun getValue(owner: Any?, slot: KProperty<*>): Int = 1\n\
                          }\n\
                          \n\
                          expect val delegated: Int by Cache()\n\
                          \n\
                          expect class C {\n\
                          \x20   val member: Int by Cache()\n\
                          }\n";
    let (ok, report) = compile(SOURCE, true);
    assert!(!ok, "the compile must fail:\n{report}");
    let expected = vec![
        format!("Main.kt:9:30: {DELEGATE}"),
        format!("Main.kt:12:24: {DELEGATE}"),
    ];
    let sources = [("Main.kt", SOURCE)];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
    );
}

/// The same rule through the stdlib's own delegate, kept separate as integration coverage: it
/// proves `by lazy { … }` reaches the check, not that the check is general.
#[test]
fn a_lazy_delegate_on_an_expect_property_is_reported_too() {
    const SOURCE: &str = "package plib\n\
                          \n\
                          expect val delegated: Int by lazy { 1 }\n";
    let (ok, report) = compile(SOURCE, true);
    assert!(!ok, "the compile must fail:\n{report}");
    let expected = vec![format!("Main.kt:3:30: {DELEGATE}")];
    let sources = [("Main.kt", SOURCE)];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
    );
}

/// A secondary constructor with a body inside an `expect class` is NOT reported — measured, and
/// the opposite of what the neighbouring shapes suggest.
#[test]
fn a_secondary_constructor_body_is_not_this_diagnostic() {
    const SOURCE: &str = "package plib\n\
                          \n\
                          expect class A {\n\
                          \x20   constructor(x: Int) { }\n\
                          }\n";
    let (_, report) = compile(SOURCE, true);
    let sources = [("Main.kt", SOURCE)];
    // The whole ledger, so "not this diagnostic" is a measured absence rather than one probe: the
    // only entry either compiler writes is the unmatched header, and it is the same entry.
    let expected = vec![format!(
        "Main.kt:3:1: error: expected A has no actual declaration in module <{MODULE}> for JVM"
    )];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
    );
}

/// Once any `expect` body exists the reference compiler never reaches actualization, so the
/// unmatched-expect report is suppressed for the WHOLE compilation — not just for the file that
/// carried the body. Measured with exactly this two-file shape.
#[test]
fn a_body_error_suppresses_the_unmatched_expect_report_everywhere() {
    let sources = [
        ("A.kt", "package plib\nexpect fun withBody(): Int = 1\n"),
        ("B.kt", "package plib\nexpect fun clean(): Int\n"),
    ];
    let (ok, report) = compile_files(&sources, true);
    assert!(!ok, "the compile must fail:\n{report}");
    // The whole ledger over BOTH files: the body error, and nothing for the clean unmatched header
    // in the other file. A probe for "has no actual declaration" could not tell that apart from a
    // report that named the wrong declaration.
    let expected = vec![format!("A.kt:2:1: {BODY}")];
    assert_eq!(kotlinc_ledger(&sources), expected, "kotlinc's whole ledger");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty reports the same ledger, entry for entry:\n{report}"
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
    const SOURCE: &str = "package plib\n\
                          \n\
                          expect fun helper(): Int\n\
                          \n\
                          expect val prop: Int\n\
                          \n\
                          expect class Holder {\n\
                          \x20   fun value(): Int\n\
                          }\n";
    let (_, report) = compile(SOURCE, true);
    let sources = [("Main.kt", SOURCE)];
    // A header declares nothing to reject, so the only entries are the unmatched-expect reports —
    // each at its own `expect` keyword. Stating all three says both halves at once: this check
    // costs an ordinary header nothing, and the report it does get is unaffected.
    let unmatched = |line: u32, name: &str| {
        format!(
            "Main.kt:{line}:1: error: expected {name} has no actual \
             declaration in module <{MODULE}> for JVM"
        )
    };
    // One measured difference, pinned by both complete ledgers rather than described: the reference
    // compiler reports every unactualized CLASSIFIER before any callable, each group in source
    // order, so `Holder` comes first there although it is written last. krusty's diagnostic sink
    // normalises the whole compilation to source order, which is a property of the sink and not of
    // this check — a grouping every diagnostic krusty writes would have to give up.
    assert_eq!(
        kotlinc_ledger(&sources),
        vec![
            unmatched(7, "Holder"),
            unmatched(3, "helper"),
            unmatched(5, "prop")
        ],
        "kotlinc's whole ledger"
    );
    assert_eq!(
        ledger(&report, &sources),
        vec![
            unmatched(3, "helper"),
            unmatched(5, "prop"),
            unmatched(7, "Holder")
        ],
        "krusty's whole ledger: the same three entries, in the sink's source order:\n{report}"
    );
}
