//! A source `by` convention and an anonymous object in the same file. Capture discovery
//! rechecks the file before a resolved index exists. The delegate's declared shape lives on
//! that index, so the scratch pass used to panic with "a selected source delegate convention
//! must retain its shape" instead of reading the candidate's own signature.

use super::common;

const SOURCE: &str = include_str!("box_regressions/delegateCaptureDiscovery.kt");

#[test]
fn source_delegate_with_anonymous_object_matches_kotlinc() {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, symbols, info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    assert!(
        !diagnostics.has_errors(),
        "{}",
        diagnostics.render("delegate.kt", SOURCE)
    );
    assert!(symbols.is_some());
    assert!(info.is_some());
    common::expect_box_same_as_kotlinc(SOURCE, "DelegateCaptureDiscovery");
}
