//! Atomic accesses through a property reference, resolved to the field it designates.
//!
//! The target stdlib declares each atomic field access as an intrinsic over a property reference,
//! and kotlinc lowers the intrinsic after inlining, once the reference is the literal the inlined
//! lambda returns. A library's atomicfu helper is such an inline function: it applies the
//! intrinsic to the result of invoking its lambda parameter. Its body lowers each access to an
//! [`IrCheckedOperation::AtomicFieldThrough`], and the caller's lambda lowers its property
//! reference to an [`IrCheckedOperation::FieldDesignation`]. After the unit expands its inline
//! calls, each access reaches the designation its expanded lambda produces and becomes the
//! [`IrCheckedOperation::AtomicField`] on that field. An access that reaches anything else, and a
//! designation no access consumes, decline the body.

use crate::fx_hash::HashSet;
use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrFile};
use crate::libraries::AtomicOperation;
use crate::types::Ty;

use super::decline::KlibBodyDeclineReason;

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// The field `reference` designates, with the statements evaluated before it: the designation
/// itself, or the value of a block (an expanded lambda's body) that ends in one.
fn designation(ir: &IrFile, mut reference: ExprId) -> Option<(Vec<ExprId>, ExprId)> {
    let mut statements = Vec::new();
    loop {
        match ir.expr(reference) {
            IrExpr::Checked(operation)
                if matches!(**operation, IrCheckedOperation::FieldDesignation { .. }) =>
            {
                return Some((statements, reference));
            }
            IrExpr::Block {
                stmts,
                value: Some(value),
            } => {
                statements.extend(stmts.iter().copied());
                reference = *value;
            }
            _ => return None,
        }
    }
}

/// Resolve the atomic access `site` to the field its reference designates, in place.
pub(super) fn resolve(ir: &mut IrFile, site: ExprId) -> Result<(), KlibBodyDeclineReason> {
    let IrExpr::Checked(access) = ir.expr(site) else {
        unreachable!("the site is an atomic access")
    };
    let IrCheckedOperation::AtomicFieldThrough {
        operation,
        reference,
        operands,
    } = (**access).clone()
    else {
        unreachable!("the site is an atomic access")
    };
    let Some((statements, designation)) = designation(ir, reference) else {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "an atomic field access through another receiver than a property reference",
        ));
    };
    let IrExpr::Checked(designated) = ir.expr(designation) else {
        unreachable!("a designation was found")
    };
    let IrCheckedOperation::FieldDesignation {
        receiver,
        class,
        index,
        ty: field,
        final_,
    } = **designated
    else {
        unreachable!("a designation was found")
    };
    if final_ && operation != AtomicOperation::Load {
        return Err(mismatch("an atomic field access writes a final field"));
    }
    let result = match operation {
        AtomicOperation::Store => Ty::Unit,
        AtomicOperation::CompareAndSet => Ty::Boolean,
        AtomicOperation::Load
        | AtomicOperation::Exchange
        | AtomicOperation::CompareAndExchange
        | AtomicOperation::FetchAndAdd => field,
    };
    let ty = ir
        .logical_type(site)
        .ok_or_else(|| mismatch("an atomic field access has no type"))?;
    if ty != result {
        return Err(mismatch(
            "an atomic field access disagrees with its field's storage type",
        ));
    }
    if operands
        .iter()
        .any(|operand| ir.logical_type(*operand) != Some(field))
    {
        return Err(mismatch(
            "an atomic field access operand disagrees with its field's storage type",
        ));
    }
    let access = IrExpr::Checked(Box::new(IrCheckedOperation::AtomicField {
        operation,
        receiver,
        class,
        index,
        operands,
    }));
    if statements.is_empty() {
        ir.exprs[site as usize] = access;
    } else {
        let access = ir.add_expr(access);
        ir.set_logical_type(access, ty);
        ir.exprs[site as usize] = IrExpr::Block {
            stmts: statements,
            value: Some(access),
        };
    }
    Ok(())
}

/// Resolve every atomic access the unit's function bodies still make through a reference, now
/// that inline calls are expanded, and decline a designation that remains: no target creates
/// a property reference to a field of a library class.
pub(super) fn resolve_all(ir: &mut IrFile) -> Result<(), KlibBodyDeclineReason> {
    let accesses = reachable(ir)
        .into_iter()
        .filter(|&expression| {
            matches!(
                ir.expr(expression),
                IrExpr::Checked(operation)
                    if matches!(**operation, IrCheckedOperation::AtomicFieldThrough { .. })
            )
        })
        .collect::<Vec<_>>();
    for access in accesses {
        resolve(ir, access)?;
    }
    if reachable(ir).into_iter().any(|expression| {
        matches!(
            ir.expr(expression),
            IrExpr::Checked(operation)
                if matches!(**operation, IrCheckedOperation::FieldDesignation { .. })
        )
    }) {
        return Err(KlibBodyDeclineReason::UnsupportedOperation(
            "a property reference used other than as an atomic field access's receiver",
        ));
    }
    Ok(())
}

/// The expressions of every function body the unit keeps, outside the inline functions it
/// expands at each call and never emits.
fn reachable(ir: &IrFile) -> Vec<ExprId> {
    let mut pending = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(function, _)| !ir.inline_only_fns.contains(&(*function as u32)))
        .filter_map(|(_, function)| function.body)
        .collect::<Vec<_>>();
    let mut seen = HashSet::default();
    let mut found = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        found.push(expression);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    found
}
