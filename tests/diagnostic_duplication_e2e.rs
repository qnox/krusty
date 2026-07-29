use krusty::diag::DiagSink;
use krusty::frontend::analyze_source_set;
use krusty::libraries::EmptySymbolSource;
use std::collections::HashMap;

fn diagnostic_counts(sources: &[&str]) -> HashMap<(u32, u32, String), usize> {
    let mut diags = DiagSink::new();
    analyze_source_set(sources, Box::new(EmptySymbolSource), &mut diags);
    let mut counts = HashMap::new();
    for diagnostic in &diags.diags {
        *counts
            .entry((diagnostic.file, diagnostic.span.lo, diagnostic.msg.clone()))
            .or_default() += 1;
    }
    counts
}

fn assert_each_reported_once(sources: &[&str]) {
    let counts = diagnostic_counts(sources);
    let repeated = counts
        .iter()
        .filter(|(_, &count)| count > 1)
        .collect::<Vec<_>>();
    assert!(repeated.is_empty(), "repeated diagnostics: {repeated:?}");
    assert!(!counts.is_empty(), "expected diagnostics, got none");
}

#[test]
fn member_return_annotation_reports_once() {
    assert_each_reported_once(&[
        "class C {\n    fun f(): Gone = TODO()\n    fun g(): Gone = TODO()\n}",
    ]);
}

#[test]
fn base_class_member_return_annotation_reports_once() {
    assert_each_reported_once(&[
        "open class A {\n    open fun f(): Gone = TODO()\n}\nclass B : A()\nclass C : A()",
    ]);
}

#[test]
fn re_checked_expression_reports_once() {
    assert_each_reported_once(&[
        "fun f(): Int {\n    var sum = 0\n    sum += (1.gone() - 48)\n    return sum\n}",
    ]);
}

#[test]
fn member_property_annotation_reports_once() {
    assert_each_reported_once(&["class C {\n    val p: Gone = TODO()\n}"]);
}
