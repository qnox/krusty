//! Call-site copies of lambdas that escape an inline expansion.
//!
//! A lambda stored by an inline function is not spliced into the caller. Its implementation method
//! is a separate function, so substituting types only in the inline template leaves that method on
//! the declaration's types. This module clones that implementation when the expansion fixes a
//! reified type the body uses, and points the copied lambda at the clone. A non-reified parameter
//! stays erased on the shared implementation. The declaration's implementation stays in place for
//! the inline method's own fallback body.

use std::collections::{HashMap, HashSet};

use crate::ir::{Callee, ExprId, IrCheckedOperation, IrCheckedSubstitution, IrExpr};
use crate::types::{ty_subst_keep_unbound, Ty};

/// Point each cloned escaping lambda at an implementation specialized for this expansion.
pub(super) fn specialize(
    ir: &mut crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    protected: &HashSet<ExprId>,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    caller_declaration: crate::fir::DeclarationId,
    caller: Option<crate::ir::IrEnclosure>,
    caller_is_default: bool,
    caller_source_name: &str,
    inline_callee: crate::fir::CallableId,
    inline_callee_source_name: &str,
) {
    if reified_bindings.is_empty() {
        return;
    }
    let mut replacements = HashMap::<u32, u32>::new();
    let mut decisions = HashMap::<u32, bool>::new();
    let mut state = Specialization {
        replacements: &mut replacements,
        decisions: &mut decisions,
        caller_declaration,
        caller,
        caller_is_default,
        caller_source_name,
        inline_callee,
        inline_callee_source_name,
    };
    let mut lambdas = copies
        .iter()
        .copied()
        .filter(|(source, _)| !protected.contains(source))
        .filter(|(_, copy)| matches!(ir.expr(*copy), IrExpr::Lambda { .. }))
        .collect::<Vec<_>>();
    let mut plan_lambdas = plan_lambda_implementations(
        ir,
        copies
            .iter()
            .filter(|(source, _)| !protected.contains(source))
            .map(|(_, copy)| *copy),
    )
    .into_iter()
    .map(|(expression, _)| (expression, expression))
    .collect::<Vec<_>>();
    lambdas.append(&mut plan_lambdas);
    lambdas.sort_unstable_by_key(|&(_, expression)| expression);
    lambdas.dedup_by_key(|(_, expression)| *expression);
    for (_, copy) in lambdas {
        let IrExpr::Lambda { impl_fn, .. } = ir.expr(copy).clone() else {
            continue;
        };
        let Some(_) =
            specialized_lambda_function(ir, impl_fn, bindings, reified_bindings, &mut state, None)
        else {
            continue;
        };
        if let Some(caller) = state.caller {
            ir.callable_reference_enclosures.insert(copy, caller);
        }
        // The copied lambda's `inline_body` is a separate expression dag from the implementation.
        // Nested lambdas there still name the declaration implementations until this retarget.
        retarget_lambdas(ir, copy, state.replacements);
    }
}

struct Specialization<'a> {
    replacements: &'a mut HashMap<u32, u32>,
    decisions: &'a mut HashMap<u32, bool>,
    caller_declaration: crate::fir::DeclarationId,
    caller: Option<crate::ir::IrEnclosure>,
    caller_is_default: bool,
    caller_source_name: &'a str,
    inline_callee: crate::fir::CallableId,
    inline_callee_source_name: &'a str,
}

fn retarget_lambdas(ir: &mut crate::ir::IrFile, root: ExprId, replacements: &HashMap<u32, u32>) {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    let mut plans = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let current = match ir.expr(expression) {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        };
        if let Some(current) = current {
            if let Some(done) = replacements.get(&current).copied() {
                if let IrExpr::Lambda { impl_fn, .. } = &mut ir.exprs[expression as usize] {
                    *impl_fn = done;
                }
            }
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

fn specialized_lambda_function(
    ir: &mut crate::ir::IrFile,
    function: u32,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    state: &mut Specialization<'_>,
    parent: Option<u32>,
) -> Option<u32> {
    if let Some(done) = state.replacements.get(&function).copied() {
        return Some(done);
    }
    // Only a reified argument is a runtime class. Cloning for an ordinary parameter would turn an
    // erased `as? T` into a check of whatever this call inferred. A use that exists only in a nested
    // implementation still counts: that implementation is a separate function, not an expression child.
    if !function_uses_binding(
        ir,
        function,
        reified_bindings,
        state.decisions,
        &mut HashSet::new(),
        &mut HashSet::new(),
    ) {
        return None;
    }
    let body = ir.functions.get(function as usize)?.body?;
    let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
    let mut local_delegate_plans = HashMap::new();
    let mut copied_expressions = cloned.values().copied().collect::<Vec<_>>();
    copied_expressions.sort_unstable();
    for copy in copied_expressions {
        super::local_delegate_plans::specialize_inline_copy(
            ir,
            copy,
            bindings,
            reified_bindings,
            &mut local_delegate_plans,
        )?;
    }
    let mut shape = ir.functions[function as usize].clone();
    shape.body = Some(cloned_body);
    shape.params = shape
        .params
        .iter()
        .copied()
        .map(|ty| ty_subst_keep_unbound(ty, bindings))
        .collect();
    shape.ret = ty_subst_keep_unbound(shape.ret, bindings);
    ir.runtime_reified_lambda_implementations.insert(function);
    let specialized = crate::ir::clone_function_implementation(
        ir,
        function,
        shape,
        bindings,
        crate::ir::IrSpecializedFunction {
            source: function,
            caller_declaration: state.caller_declaration,
            caller: state.caller,
            caller_is_default: state.caller_is_default,
            caller_source_name: state.caller_source_name.to_string(),
            inline_callee: state.inline_callee,
            inline_callee_source_name: state.inline_callee_source_name.to_string(),
            parent,
        },
    );
    ir.runtime_reified_lambda_implementations
        .insert(specialized);
    for expression in cloned.values().copied() {
        let Some(enclosure) = ir.callable_reference_enclosures.get_mut(&expression) else {
            continue;
        };
        *enclosure = match *enclosure {
            crate::ir::IrEnclosure::Function(owner) if owner == function => {
                crate::ir::IrEnclosure::Function(specialized)
            }
            crate::ir::IrEnclosure::Lambda(owner) if owner == function => {
                crate::ir::IrEnclosure::Lambda(specialized)
            }
            other => other,
        };
    }
    // Record the copy before descending so a cycle retargets to this function instead of cloning it again.
    state.replacements.insert(function, specialized);
    let nested = lambda_implementations(ir, cloned_body);
    for (expression, implementation) in nested {
        let Some(done) = specialized_lambda_function(
            ir,
            implementation,
            bindings,
            reified_bindings,
            state,
            Some(specialized),
        ) else {
            continue;
        };
        if let IrExpr::Lambda { impl_fn, .. } = &mut ir.exprs[expression as usize] {
            *impl_fn = done;
        }
    }
    remap_implementation_provenance(ir, cloned_body, state.replacements);
    Some(specialized)
}

fn function_uses_binding(
    ir: &crate::ir::IrFile,
    function: u32,
    bindings: &HashMap<String, Ty>,
    decisions: &mut HashMap<u32, bool>,
    functions: &mut HashSet<u32>,
    plans: &mut HashSet<u32>,
) -> bool {
    if bindings.is_empty() {
        return false;
    }
    if let Some(done) = decisions.get(&function).copied() {
        return done;
    }
    if !functions.insert(function) {
        return false;
    }
    let used = ir
        .functions
        .get(function as usize)
        .and_then(|function| function.body)
        .is_some_and(|body| dag_uses_binding(ir, body, bindings, functions, decisions, plans));
    decisions.insert(function, used);
    used
}

fn dag_uses_binding(
    ir: &crate::ir::IrFile,
    root: ExprId,
    bindings: &HashMap<String, Ty>,
    functions: &mut HashSet<u32>,
    decisions: &mut HashMap<u32, bool>,
    plans: &mut HashSet<u32>,
) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if node_uses_binding(ir.expr(expression), bindings)
            || expression_facts_use_binding(ir, expression, bindings)
        {
            return true;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            if function_uses_binding(ir, *impl_fn, bindings, decisions, functions, plans) {
                return true;
            }
        }
        if let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) {
            if local_delegate_plan_uses_binding(
                ir,
                access.plan,
                bindings,
                functions,
                decisions,
                plans,
            ) {
                return true;
            }
        }
    }
    false
}

fn local_delegate_plan_uses_binding(
    ir: &crate::ir::IrFile,
    plan: u32,
    bindings: &HashMap<String, Ty>,
    functions: &mut HashSet<u32>,
    decisions: &mut HashMap<u32, bool>,
    plans: &mut HashSet<u32>,
) -> bool {
    if !plans.insert(plan) {
        return false;
    }
    let Some(plan) = ir.local_delegate_plans.get(plan as usize) else {
        return false;
    };
    std::iter::once(plan.getter.body)
        .chain(plan.setter.as_ref().map(|setter| setter.body))
        .any(|body| dag_uses_binding(ir, body, bindings, functions, decisions, plans))
}

/// Markers and inline-lambda receivers name an implementation by [`FunId`]. After a nested
/// implementation is copied, those edges follow the copy. Source class provenance stays on the
/// copied [`crate::ir::IrLambdaOrigin`].
fn remap_implementation_provenance(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    replacements: &HashMap<u32, u32>,
) {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    let mut plans = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(provenance) = ir.debug_local_provenance(expression) {
            if let Some(remapped) = remap_provenance(provenance, replacements) {
                ir.set_debug_local_provenance(expression, remapped);
            }
        }
        if let IrExpr::Try { catches, .. } = ir.expr(expression).clone() {
            let mut catches = catches;
            let mut changed = false;
            for catch in &mut catches {
                let Some(binding) = catch.binding.as_mut() else {
                    continue;
                };
                let Some(provenance) = binding.provenance else {
                    continue;
                };
                let Some(remapped) = remap_provenance(provenance, replacements) else {
                    continue;
                };
                binding.provenance = Some(remapped);
                changed = true;
            }
            if changed {
                if let IrExpr::Try { catches: slot, .. } = &mut ir.exprs[expression as usize] {
                    *slot = catches;
                }
            }
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

fn remap_provenance(
    provenance: crate::ir::IrDebugLocalProvenance,
    replacements: &HashMap<u32, u32>,
) -> Option<crate::ir::IrDebugLocalProvenance> {
    match provenance {
        crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver { implementation } => replacements
            .get(&implementation)
            .copied()
            .map(
                |implementation| crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver {
                    implementation,
                },
            ),
        crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
            implementation,
            depth,
        } => replacements
            .get(&implementation)
            .copied()
            .map(
                |implementation| crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
                    implementation,
                    depth,
                },
            ),
        crate::ir::IrDebugLocalProvenance::InlineValue { .. }
        | crate::ir::IrDebugLocalProvenance::FunctionFrameMarker => None,
    }
}

fn lambda_implementations(ir: &crate::ir::IrFile, root: ExprId) -> Vec<(ExprId, u32)> {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    let mut plans = HashSet::new();
    let mut implementations = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            implementations.push((expression, *impl_fn));
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    implementations
}

fn plan_lambda_implementations(
    ir: &crate::ir::IrFile,
    roots: impl IntoIterator<Item = ExprId>,
) -> Vec<(ExprId, u32)> {
    let mut pending = Vec::new();
    let mut plans = HashSet::new();
    for root in roots {
        enqueue_local_delegate_plan_bodies(ir, root, &mut pending, &mut plans);
    }
    let mut seen = HashSet::new();
    let mut implementations = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::Lambda {
                impl_fn, captures, ..
            } => {
                implementations.push((expression, *impl_fn));
                pending.extend(captures.iter().copied());
            }
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
    }
    implementations
}

fn enqueue_local_delegate_plan_bodies(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    pending: &mut Vec<ExprId>,
    plans: &mut HashSet<u32>,
) {
    let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) else {
        return;
    };
    if !plans.insert(access.plan) {
        return;
    }
    let Some(plan) = ir.local_delegate_plans.get(access.plan as usize) else {
        return;
    };
    pending.push(plan.getter.body);
    pending.extend(plan.setter.as_ref().map(|setter| setter.body));
}

fn uses_binding(ty: Ty, bindings: &HashMap<String, Ty>) -> bool {
    ty_subst_keep_unbound(ty, bindings) != ty
}

fn substitution_uses_binding(
    substitution: &IrCheckedSubstitution,
    bindings: &HashMap<String, Ty>,
) -> bool {
    substitution.reified
        && (uses_binding(substitution.value, bindings)
            || substitution
                .additional_bounds
                .iter()
                .copied()
                .any(|bound| uses_binding(bound, bindings)))
}

fn substitutions_use_binding(
    substitutions: &[IrCheckedSubstitution],
    bindings: &HashMap<String, Ty>,
) -> bool {
    substitutions
        .iter()
        .any(|substitution| substitution_uses_binding(substitution, bindings))
}

/// Runtime call facts that carry a reified type argument. Static result/signature/storage facts are
/// deliberately excluded: specializing those types may be necessary once a copy exists, but they
/// do not cause a closure implementation to execute a reified operation.
fn expression_facts_use_binding(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
) -> bool {
    if ir
        .reified_call_subst
        .get(&expression)
        .is_some_and(|substitutions| {
            substitutions
                .iter()
                .any(|(_, ty)| uses_binding(*ty, bindings))
        })
    {
        return true;
    }
    match ir.expr(expression) {
        IrExpr::Call {
            callee: Callee::External { substitutions, .. },
            ..
        } => substitutions_use_binding(substitutions, bindings),
        IrExpr::Checked(operation) => checked_substitutions(operation)
            .is_some_and(|substitutions| substitutions_use_binding(substitutions, bindings)),
        IrExpr::ReifiedTypeOp { name, .. } | IrExpr::ReifiedClassMarker { name, .. } => {
            bindings.contains_key(name)
        }
        _ => false,
    }
}

fn checked_substitutions(operation: &IrCheckedOperation) -> Option<&[IrCheckedSubstitution]> {
    match operation {
        IrCheckedOperation::Call { substitutions, .. }
        | IrCheckedOperation::ConstructorDelegation { substitutions, .. }
        | IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => Some(substitutions),
        IrCheckedOperation::PropertyReference { .. }
        | IrCheckedOperation::ExternalPropertyRead { .. }
        | IrCheckedOperation::ExternalPropertyWrite { .. }
        | IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. }
        | IrCheckedOperation::RangeConstruction { .. }
        | IrCheckedOperation::RangeContains { .. }
        | IrCheckedOperation::IllegalProgressionStep { .. }
        | IrCheckedOperation::RangeLoop { .. } => None,
    }
}

/// Whether this node carries a runtime type operation the reified map would change.
///
/// Static signature slots are not a reason to copy. [`super::specialize_typed_expression`] applies
/// the same split when the copy is built, so detection does not clone the expression to compare it.
fn node_uses_binding(expression: &IrExpr, bindings: &HashMap<String, Ty>) -> bool {
    match expression {
        IrExpr::TypeOp { type_operand, .. } => uses_binding(*type_operand, bindings),
        IrExpr::KClassLiteral { classifier, .. } => {
            classifier.is_some_and(|ty| uses_binding(ty, bindings))
        }
        IrExpr::Call {
            callee: Callee::Intrinsic { operation, .. },
            ..
        } => match operation {
            crate::ir::IrIntrinsic::TypeOf { ty } => uses_binding(*ty, bindings),
            _ => false,
        },
        _ => false,
    }
}
