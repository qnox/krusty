//! A `+` chain whose every leaf is a string literal or a string template.
//!
//! kotlinc's raw FIR builder flattens that chain into one string-concatenation call before
//! resolution (`isFoldedStrings`). A name, a field, or any other operand keeps the ordinary
//! `plus` call. Metadata treats only the concatenation as a constant when each part is one.

use crate::ast::{BinOp, Expr, ExprId, File};

/// The string leaves of `expression`, or `None` when any leaf is not a string literal or template.
pub(super) fn leaves(file: &File, expression: ExprId) -> Option<Vec<ExprId>> {
    match file.expr(expression) {
        Expr::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
            ..
        } => {
            let mut parts = flatten(file, *lhs)?;
            parts.extend(flatten(file, *rhs)?);
            Some(parts)
        }
        _ => None,
    }
}

fn flatten(file: &File, expression: ExprId) -> Option<Vec<ExprId>> {
    match file.expr(expression) {
        Expr::StringLit(_) | Expr::Template(_) => Some(vec![expression]),
        Expr::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
            ..
        } => {
            let mut parts = flatten(file, *lhs)?;
            parts.extend(flatten(file, *rhs)?);
            Some(parts)
        }
        _ => None,
    }
}
