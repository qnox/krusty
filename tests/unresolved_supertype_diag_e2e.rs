//! Unresolved supertypes report the first failed segment at the supertype's source span.

use super::common;
use krusty::diag::DiagSink;
use krusty::frontend::analyze_source_set;
use krusty::libraries::EmptySymbolSource;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

#[test]
fn simple_missing_supertype_reports_name_at_use_site() {
    let ds = diags("class Foo : TotallyMissing {\n  fun b() = 2\n}\n");
    assert_eq!(
        ds,
        vec!["unresolved reference 'TotallyMissing'.".to_string()]
    );
}

#[test]
fn qualified_missing_supertype_reports_first_failing_segment() {
    let ds = diags("class Bar : deep.pkg.Missing3 {\n  fun c() = 3\n}\n");
    assert_eq!(ds, vec!["unresolved reference 'deep'.".to_string()]);
}

#[test]
fn existing_package_missing_class_reports_class_segment() {
    let ds = diags("class B : kotlin.Missing5 {\n  fun b() = 2\n}\n");
    assert_eq!(ds, vec!["unresolved reference 'Missing5'.".to_string()]);
}

#[test]
fn existing_outer_missing_nested_reports_nested_segment() {
    let ds = diags("class Outer\nclass D : Outer.Nope\n");
    assert_eq!(ds, vec!["unresolved reference 'Nope'.".to_string()]);
}

#[test]
fn each_missing_supertype_is_reported_once() {
    let ds = diags("class E : Missing1, Missing2()\n");
    assert_eq!(
        ds,
        vec![
            "unresolved reference 'Missing1'.".to_string(),
            "unresolved reference 'Missing2'.".to_string(),
        ]
    );
}

#[test]
fn qualified_supertype_uses_the_failed_segment_span() {
    let mut diagnostics = DiagSink::new();
    analyze_source_set(
        &["class Bar : deep.pkg.Missing3\n"],
        Box::new(EmptySymbolSource),
        &mut diagnostics,
    );
    let observed = diagnostics
        .diags
        .into_iter()
        .map(|diagnostic| {
            (
                diagnostic.file,
                diagnostic.span.lo,
                diagnostic.span.hi,
                diagnostic.msg,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![(0, 12, 16, "unresolved reference 'deep'.".to_string(),)]
    );
}

#[test]
fn failed_segment_is_recorded_per_file() {
    let diagnostics = common::front_end_diagnostics_files(
        &[
            "package a\nclass Outer\nfun f(value: Outer.Nope) = 0\n",
            "package b\nfun g(value: Outer.Nope) = 0\n",
        ],
        &[],
        None,
    );
    assert_eq!(
        diagnostics,
        vec![
            "unresolved reference 'Nope'.",
            "unresolved reference 'Outer'.",
        ]
    );
}
