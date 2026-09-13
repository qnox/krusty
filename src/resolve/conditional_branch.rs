//! Conditional-branch value discovery used before rebinding a generic result against its sibling.

use crate::ast::{Expr, ExprId, File};

/// Return the expression whose value a conditional branch produces.
///
/// A branch written as a block (`if (c) { … } else { … }`) yields its trailing expression, and it
/// is that expression's call—not the block—that carries the generic signature a sibling branch can
/// rebind. Nested blocks are transparent; a block with no trailing expression produces no value and
/// remains the returned expression.
pub(super) fn branch_value_expression(file: &File, expression: ExprId) -> ExprId {
    let mut current = expression;
    loop {
        let Expr::Block { trailing, .. } = file.expr(current) else {
            return current;
        };
        match trailing {
            Some(trailing) => current = *trailing,
            None => return current,
        }
    }
}
