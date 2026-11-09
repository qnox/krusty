//! Common lowering for primitive equality with a safe-call operand.
//!
//! `a?.x == y`, where `y` is a non-null primitive, folds into the safe call's guard: the
//! comparison becomes the non-null branch and the null branch answers `false`. The safe call on
//! the right folds the same way, which moves the left operand into the non-null branch: it is not
//! evaluated when the receiver is `null`. That source evaluation order is a common semantic fact,
//! not a backend optimization. `!=` is the negation of the folded equality.
//!
//! An immutable receiver binding may be read directly once moving the other operand into the
//! guarded branch leaves the temporary redundant. This preserves source evaluation order while
//! leaving the common IR in the same canonical shape for every backend.

use crate::ir::{ExprId, IrBindingStability, IrConst, IrExpr, IrFile};
use crate::types::{EqualityMode, Ty};

pub(super) fn fold_safe_call_equalities(ir: &mut IrFile) {
    // Operands precede the expressions that use them, so a nested equality folds first.
    let equalities = (0..ir.exprs.len())
        .filter_map(|index| {
            let id = ExprId::try_from(index).expect("too many expressions");
            matches!(
                ir.expr(id),
                IrExpr::Equality {
                    mode: EqualityMode::Structural,
                    ..
                }
            )
            .then_some(id)
        })
        .collect::<Vec<_>>();
    for equality in equalities {
        fold(ir, equality);
    }
}

fn fold(ir: &mut IrFile, equality: ExprId) {
    let IrExpr::Equality { op, lhs, rhs, .. } = *ir.expr(equality) else {
        return;
    };
    let folded = |safe_call: ExprId, other: ExprId| {
        is_non_null_primitive(ir, other)
            .then(|| safe_call_guard(ir, safe_call))
            .flatten()
    };
    let (block, other, safe_call_first) = match (folded(lhs, rhs), folded(rhs, lhs)) {
        (Some(guard), _) => (guard, rhs, true),
        (None, Some(guard)) => (guard, lhs, false),
        (None, None) => return,
    };
    let SafeCallGuard {
        block,
        guard,
        condition,
        selector,
    } = block;
    if !safe_call_first {
        inline_stable_receiver(ir, block, &[condition, selector]);
    }
    let result = safe_call_result(ir, selector);
    let (left, right) = if safe_call_first {
        (result, other)
    } else {
        (other, result)
    };
    let mode = match (ir.checked_type(left), ir.checked_type(right)) {
        (Some(left), Some(right)) => EqualityMode::of_source_operands(left, right),
        _ => EqualityMode::Structural,
    };
    let compared = ir.add_expr(IrExpr::Equality {
        op: crate::ir::IrBinOp::Eq,
        mode,
        lhs: left,
        rhs: right,
    });
    // The comparison keeps the equality's own source position inside the branch.
    if let Some(line) = ir.source_line(equality) {
        ir.set_source_line(compared, line);
    }
    if let Some(line) = ir.expr_lines.get(&equality).copied() {
        ir.expr_lines.insert(compared, line);
    }
    let absent = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
    ir.exprs[guard as usize] = IrExpr::When {
        branches: vec![(Some(condition), absent), (None, compared)],
    };
    ir.whens.exhaustive.insert(guard, Ty::Boolean);
    ir.set_logical_type(guard, Ty::Boolean);
    ir.set_logical_type(block, Ty::Boolean);
    if op == crate::ir::IrBinOp::Ne {
        ir.replace_with_negation(equality, block);
    } else {
        ir.exprs[equality as usize] = IrExpr::Block {
            stmts: Vec::new(),
            value: Some(block),
        };
        ir.set_logical_type(equality, Ty::Boolean);
    }
}

/// Read the binding a safe call's receiver temporary copies in place of the temporary when that
/// binding is immutable.
fn inline_stable_receiver(ir: &mut IrFile, block: ExprId, uses: &[ExprId]) {
    let IrExpr::Block { stmts, .. } = ir.expr(block) else {
        return;
    };
    let [declaration] = stmts.as_slice() else {
        return;
    };
    let declaration = *declaration;
    let IrExpr::Variable {
        index: temporary,
        init: Some(init),
        named: false,
        ..
    } = *ir.expr(declaration)
    else {
        return;
    };
    let IrExpr::GetValue(binding) = *ir.expr(init) else {
        return;
    };
    if ir.binding_read_stability.get(&init) != Some(&IrBindingStability::Stable) {
        return;
    }
    let mut reads = Vec::new();
    let mut pending = uses.to_vec();
    while let Some(expression) = pending.pop() {
        match ir.expr(expression) {
            IrExpr::GetValue(value) if *value == temporary => reads.push(expression),
            // A nested body has its own value numbering.
            IrExpr::Lambda { .. } => return,
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    for read in reads {
        ir.exprs[read as usize] = IrExpr::GetValue(binding);
        ir.binding_read_stability
            .insert(read, IrBindingStability::Stable);
    }
    if let IrExpr::Block { stmts, .. } = &mut ir.exprs[block as usize] {
        stmts.clear();
    }
}

/// A safe call's parts: its block, the guarding `when`, the null test and the non-null selector.
struct SafeCallGuard {
    block: ExprId,
    guard: ExprId,
    condition: ExprId,
    selector: ExprId,
}

/// `expression` as a safe call whose null branch answers `null`.
fn safe_call_guard(ir: &IrFile, expression: ExprId) -> Option<SafeCallGuard> {
    let IrExpr::Block {
        value: Some(guard), ..
    } = ir.expr(expression)
    else {
        return None;
    };
    if !ir.null_guards.contains(guard) {
        return None;
    }
    let IrExpr::When { branches } = ir.expr(*guard) else {
        return None;
    };
    let [(Some(condition), null_result), (None, selector)] = branches.as_slice() else {
        return None;
    };
    matches!(ir.expr(*null_result), IrExpr::Const(IrConst::Null)).then_some(SafeCallGuard {
        block: expression,
        guard: *guard,
        condition: *condition,
        selector: *selector,
    })
}

/// The selector's own value: the widening to the safe call's nullable result is not part of it.
fn safe_call_result(ir: &IrFile, selector: ExprId) -> ExprId {
    match *ir.expr(selector) {
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if ir
            .checked_type(arg)
            .is_some_and(|value| Ty::nullable(value) == type_operand) =>
        {
            arg
        }
        _ => selector,
    }
}

/// Whether this expression has a non-null primitive semantic type.
fn is_non_null_primitive(ir: &IrFile, expression: ExprId) -> bool {
    ir.checked_type(expression).is_some_and(|ty| {
        matches!(
            ty,
            Ty::Boolean
                | Ty::Byte
                | Ty::Short
                | Ty::Int
                | Ty::Long
                | Ty::Char
                | Ty::Float
                | Ty::Double
        )
    })
}
