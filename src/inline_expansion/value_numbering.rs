//! Value numbering across an inline boundary: the value indices an expression names, and their
//! rebasing from the inline function's numbering into its caller's.

use crate::fx_hash::HashSet;

use crate::ir::{ExprId, IrCheckedOperation, IrExpr};

/// The value indices `expression` declares, reads or writes.
pub(crate) fn value_indices(expression: &IrExpr) -> Vec<u32> {
    match expression {
        IrExpr::GetValue(index)
        | IrExpr::SetValue { var: index, .. }
        | IrExpr::Variable { index, .. } => vec![*index],
        IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var).collect(),
        IrExpr::Checked(checked_op)
            if let IrCheckedOperation::RangeLoop { variable, .. } = &**checked_op =>
        {
            vec![*variable]
        }
        _ => Vec::new(),
    }
}

/// Add `root` and every expression below it to `marked`.
pub(crate) fn mark_subtree(ir: &crate::ir::IrFile, root: ExprId, marked: &mut HashSet<ExprId>) {
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        if !marked.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

fn rebase_index(
    index: &mut u32,
    parameter_count: u32,
    operands: &[Option<u32>],
    local_base: u32,
) -> Option<()> {
    *index = if *index < parameter_count {
        // A parameter with no slot is one an inline lambda supplies. Every reference to it was
        // replaced by that lambda before rebasing, so reaching one here is a reference the
        // replacement did not see and the expansion cannot be completed.
        operands.get(*index as usize).copied().flatten()?
    } else {
        local_base.checked_add(*index - parameter_count)?
    };
    Some(())
}

/// Renumber the values `expression` names from the inline function's numbering into its
/// caller's: parameter `i` to `operands[i]`, and each local after the parameters from
/// `local_base` on.
pub(crate) fn rebase_values(
    expression: &mut IrExpr,
    parameter_count: u32,
    operands: &[Option<u32>],
    local_base: u32,
) -> Option<()> {
    match expression {
        IrExpr::GetValue(index)
        | IrExpr::SetValue { var: index, .. }
        | IrExpr::Variable { index, .. } => {
            rebase_index(index, parameter_count, operands, local_base)?
        }
        IrExpr::Try { catches, .. } => {
            for catch in catches {
                rebase_index(&mut catch.var, parameter_count, operands, local_base)?;
            }
        }
        IrExpr::Checked(checked_op) => {
            if let IrCheckedOperation::RangeLoop { variable, .. } = &mut **checked_op {
                rebase_index(variable, parameter_count, operands, local_base)?;
            }
        }
        _ => {}
    }
    Some(())
}

#[cfg(test)]
mod value_rebasing_tests {
    use super::{rebase_values, value_indices};
    use crate::fir::FirRangeOperation;
    use crate::ir::{IrCheckedOperation, IrExpr};
    use crate::types::Ty;

    #[test]
    fn a_checked_range_loop_reserves_and_rebases_its_declared_value() {
        let mut expression = IrExpr::Checked(Box::new(IrCheckedOperation::RangeLoop {
            variable: 4,
            variable_name: Some("element".into()),
            counter: Ty::Int,
            source: crate::ir::IrProgressionSource::Literal {
                operation: FirRangeOperation::Until,
                start: 0,
                end: 1,
            },
            unsigned_compare: None,
            body: 2,
            label: "loop".to_string(),
            with_index: None,
        }));

        assert_eq!(value_indices(&expression), vec![4]);
        assert_eq!(rebase_values(&mut expression, 2, &[], 20), Some(()));
        assert_eq!(value_indices(&expression), vec![22]);
    }
}
