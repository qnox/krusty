//! The checker's published verdict on whether fir2ir types an `if` or `when` by its checked result
//! (`isDeeplyProperlyExhaustive`), which common lowering copies without inspecting the branches.

use super::test_support::checked_function_body;
use super::*;

/// The published verdict of every `if` and `when` in `function`, innermost first.
fn verdicts(function: &str) -> Vec<(&'static str, bool)> {
    let (body, _) =
        checked_function_body(&format!("fun next(x: Int): Int = x + 1\n{function}\n"), "f");
    (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match expression.kind {
            FirExprKind::Conditional {
                deeply_exhaustive, ..
            } => Some(("if", deeply_exhaustive)),
            FirExprKind::When {
                deeply_exhaustive, ..
            } => Some(("when", deeply_exhaustive)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_else_if_chain_ending_in_an_else_is_deeply_exhaustive() {
    assert_eq!(
        verdicts("fun f(x: Int) { if (x > 2) next(x) else if (x > 1) next(x) else x }"),
        [("if", true), ("if", true)]
    );
}

#[test]
fn an_else_if_chain_without_a_final_else_is_not() {
    assert_eq!(
        verdicts("fun f(x: Int) { if (x > 2) next(x) else if (x > 1) next(x) }"),
        [("if", false), ("if", false)]
    );
}

#[test]
fn a_braced_else_does_not_continue_the_chain() {
    assert_eq!(
        verdicts("fun f(x: Int) { if (x > 2) next(x) else { if (x > 1) next(x) } }"),
        [("if", false), ("if", true)]
    );
}

#[test]
fn a_when_whose_else_is_an_open_else_if_is_not() {
    assert_eq!(
        verdicts("fun f(x: Int) { when { x > 2 -> next(x); else -> if (x > 1) next(x) } }"),
        [("if", false), ("when", false)]
    );
    assert_eq!(
        verdicts("fun f(x: Int) { when { x > 2 -> next(x); else -> x } }"),
        [("when", true)]
    );
}

#[test]
fn a_when_without_an_else_is_deeply_exhaustive_only_when_proven_exhaustive() {
    assert_eq!(
        verdicts("fun f(x: Boolean): Int = when (x) { true -> 1; false -> 2 }"),
        [("when", true)]
    );
    assert_eq!(
        verdicts("fun f(x: Int) { when (x) { 1 -> next(x) } }"),
        [("when", false)]
    );
}

#[test]
fn a_unit_valued_when_proven_exhaustive_without_an_else_is_deeply_exhaustive() {
    // The resolver's verdict, not the `Unit` result, decides.
    assert_eq!(
        verdicts("fun f(x: Boolean) { when (x) { true -> {}; false -> {} } }"),
        [("when", true)]
    );
    assert_eq!(
        verdicts("fun f(x: Boolean) { when (x) { true -> {} } }"),
        [("when", false)]
    );
}
