//! Inline-call copies of checked local delegated-property accessor templates.
//!
//! A local-delegate plan is executable template state, not declaration state. When its access is
//! copied across an inline boundary, the accessor bodies must follow that copy and use the same
//! split between static type substitution and runtime-reified substitution.

use std::collections::HashMap;

use crate::ir::{ExprId, IrExpr};
use crate::types::Ty;

/// Specialize one node copied across an inline boundary and retarget any delegated-property plan
/// it names. One plan copy is shared by all accesses in this expansion.
pub(super) fn specialize_inline_copy(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
    local_delegate_plans: &mut HashMap<u32, u32>,
) -> Option<()> {
    super::specialize_recorded_facts(ir, expression, bindings, runtime);
    {
        let expression = ir.exprs.get_mut(expression as usize)?;
        super::specialize_typed_expression(expression, bindings, runtime);
        super::specialize_dependency_substitutions(expression, runtime);
    }
    let source_plan = match ir.expr(expression) {
        IrExpr::LocalDelegateAccess(access) => Some(access.plan),
        _ => None,
    };
    let Some(source_plan) = source_plan else {
        return Some(());
    };
    let specialized =
        clone_specialized_plan(ir, source_plan, bindings, runtime, local_delegate_plans)?;
    let IrExpr::LocalDelegateAccess(access) = ir.exprs.get_mut(expression as usize)? else {
        return None;
    };
    access.plan = specialized;
    Some(())
}

fn clone_specialized_plan(
    ir: &mut crate::ir::IrFile,
    source_plan: u32,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
    copies: &mut HashMap<u32, u32>,
) -> Option<u32> {
    if let Some(copy) = copies.get(&source_plan).copied() {
        return Some(copy);
    }
    let mut plan = ir.local_delegate_plans.get(source_plan as usize)?.clone();
    let copy = u32::try_from(ir.local_delegate_plans.len()).ok()?;
    // Publish the identity before cloning accessor bodies so a malformed recursive plan cannot
    // recurse forever. The placeholder is replaced before this helper returns.
    ir.local_delegate_plans.push(plan.clone());
    ir.inline_local_delegate_plan_copies.insert(copy);
    copies.insert(source_plan, copy);

    super::specialize_ty(&mut plan.reference.property_type, bindings);
    specialize_accessor(ir, &mut plan.getter, bindings, runtime, copies)?;
    if let Some(setter) = plan.setter.as_mut() {
        specialize_accessor(ir, setter, bindings, runtime, copies)?;
    }
    *ir.local_delegate_plans.get_mut(copy as usize)? = plan;
    Some(copy)
}

fn specialize_accessor(
    ir: &mut crate::ir::IrFile,
    accessor: &mut crate::ir::IrLocalDelegateAccessorPlan,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
    plans: &mut HashMap<u32, u32>,
) -> Option<()> {
    let (body, cloned) = crate::ir::clone_expression_dag(ir, accessor.body);
    accessor.body = body;
    let mut expressions = cloned.values().copied().collect::<Vec<_>>();
    expressions.sort_unstable();
    for expression in expressions {
        specialize_inline_copy(ir, expression, bindings, runtime, plans)?;
    }
    super::specialize_tys(&mut accessor.parameters, bindings);
    super::specialize_ty(&mut accessor.result, bindings);
    for parameter in &mut accessor.type_parameters {
        for (bound, _) in &mut parameter.bounds {
            super::specialize_ty(bound, bindings);
        }
    }
    Some(())
}
