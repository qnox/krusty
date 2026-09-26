//! The type common lowering records for a `when` or `if`, as fir2ir types it: its checked result
//! when it is exhaustive down an unbraced `else if` chain, `Unit` otherwise.

use super::tests::lower_single_source;
use crate::ir::IrExpr;
use crate::types::Ty;

/// The recorded type of every `when` in `function`'s file, innermost first.
fn when_types(function: &str) -> Vec<Option<Ty>> {
    let ir = lower_single_source(
        &format!("fun next(x: Int): Int = x + 1\n{function}\n"),
        "WhenResultTypes",
    );
    (0..ir.exprs.len() as u32)
        .filter(|expression| matches!(ir.expr(*expression), IrExpr::When { .. }))
        .map(|expression| ir.exhaustive_whens.get(&expression).copied())
        .collect()
}

#[test]
fn an_exhaustive_else_if_chain_keeps_its_checked_type() {
    assert_eq!(
        when_types("fun f(x: Int) { if (x > 2) next(x) else if (x > 1) next(x) else x }"),
        [None, None]
    );
}

#[test]
fn an_else_if_chain_without_a_final_else_is_unit() {
    assert_eq!(
        when_types("fun f(x: Int) { if (x > 2) next(x) else if (x > 1) next(x) }"),
        [Some(Ty::Unit), Some(Ty::Unit)]
    );
}

#[test]
fn a_braced_else_is_not_an_else_if_chain() {
    assert_eq!(
        when_types("fun f(x: Int) { if (x > 2) next(x) else { if (x > 1) next(x) } }"),
        [Some(Ty::Unit), None]
    );
}

#[test]
fn a_when_ending_in_an_open_else_if_is_unit() {
    assert_eq!(
        when_types("fun f(x: Int) { when { x > 2 -> next(x); else -> if (x > 1) next(x) } }"),
        [Some(Ty::Unit), Some(Ty::Unit)]
    );
    assert_eq!(
        when_types("fun f(x: Int) { when { x > 2 -> next(x); else -> x } }"),
        [Some(Ty::Int)]
    );
}
