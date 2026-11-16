//! `if (condition) value ?: return else alternative` is one if-expression. `else` belongs to the
//! `if`, so the elvis `return` has no value. The same file's anonymous object is what capture
//! discovery checks while opening a module; a parse error on this `if` used to drop the file's
//! signatures and panic the checker.

use super::common;

const SOURCE: &str = include_str!("box_regressions/elvisReturnIfElse.kt");

#[test]
fn elvis_return_before_if_else_matches_kotlinc() {
    // Inspection analyzes the whole file, including the anonymous object, while discovering
    // captures. A parse error on the `if` used to skip the file's signatures and panic there.
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, symbols, info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    assert!(
        !diagnostics.has_errors(),
        "{}",
        diagnostics.render("select.kt", SOURCE)
    );
    assert!(symbols.is_some());
    assert!(info.is_some());
    common::expect_box_same_as_kotlinc(SOURCE, "ElvisReturnIfElse");
}
