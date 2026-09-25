//! An `expect` declaration that carries an implementation is an error.
//!
//! A header declares; it does not implement. krusty used to misdiagnose every one of these shapes
//! as "no actual declaration", which sent a reader looking for a missing `actual` rather than at
//! the body they had written.
//!
//! Every ledger below is kotlinc's own, recorded per Kotlin version in
//! `tests/recorded/expect_declaration_body_e2e.txt` (see `tests/common/recorded.rs`). Its positions
//! include three that a guess
//! would have got wrong: a property INITIALIZER is reported under the initializer expression rather
//! than at the property, an `init` block is reported at the `init` keyword rather than at its `{`,
//! and a secondary constructor with a body inside an `expect class` is not reported at all.

use super::common;

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
    let mut entries = Vec::new();
    for line in report.lines() {
        // A line that does not NAME one of this fixture's files is not a diagnostic header: it is
        // the source echo, a caret run, or a continuation. Those are skipped structurally.
        let Some((path, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(name) = path
            .rsplit('/')
            .next()
            .filter(|name| sources.iter().any(|(source, _)| source == name))
        else {
            continue;
        };
        // From here the line is a header for a file under test, so it MUST parse. Dropping one
        // silently — which `filter_map` did — is how a complete-ledger comparison passes on a
        // report it could not read: a diagnostic in an unexpected shape simply disappeared.
        let entry = rest
            .split_once(':')
            .and_then(|(line_no, rest)| {
                let (column, message) = rest.split_once(": ")?;
                line_no.parse::<u32>().ok()?;
                column.parse::<u32>().ok()?;
                Some(format!("{name}:{line_no}:{column}: {message}"))
            })
            .unwrap_or_else(|| panic!("a diagnostic header for {name} could not be read: {line}"));
        if !echoes.contains(&entry.as_str()) {
            entries.push(entry);
        }
    }
    entries
}

/// The reference compiler's ledger for the same sources, recorded per Kotlin version by the tests
/// below, so every expectation is measured rather than typed.
fn kotlinc_ledger(sources: &[(&str, &str)]) -> Vec<String> {
    kotlinc_ledger_with(sources, true)
}

fn kotlinc_ledger_with(sources: &[(&str, &str)], multiplatform: bool) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let out = dir.join("kotlinc-out");
    let mut args = vec!["-nowarn".to_string()];
    if multiplatform {
        args.push("-Xmulti-platform".to_string());
    }
    args.extend(["-d".to_string(), out.to_string_lossy().into_owned()]);
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
    let sources = [("Main.kt", SOURCE)];
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    let sources = [("Main.kt", SOURCE)];
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    let sources = [("Main.kt", SOURCE)];
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    let sources = [("Main.kt", SOURCE)];
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    let sources = [("Main.kt", SOURCE)];
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    let expected = common::recorded(|| kotlinc_ledger(&sources));
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
    const SOURCE: &str = "package plib\n\nexpect fun exprBody(): Int = 1\n";
    let (ok, report) = compile(SOURCE, false);
    assert!(!ok, "the compile must fail:\n{report}");
    let sources = [("Main.kt", SOURCE)];
    let expected = common::recorded(|| kotlinc_ledger_with(&sources, false));
    assert_eq!(
        expected.len(),
        2,
        "kotlinc's ledger: the feature gate, then the body"
    );
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "the whole ledger, in order: the feature gate first, then the body:\n{report}"
    );
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
    let expected = common::recorded(|| kotlinc_ledger(&sources));
    assert_eq!(expected.len(), 3, "kotlinc's whole ledger: {expected:?}");
    assert_eq!(
        ledger(&report, &sources),
        expected,
        "krusty's complete ordered ledger must match kotlinc:\n{report}"
    );
}
