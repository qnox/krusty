//! Inline-call copies of checked local delegated-property accessor templates.
//!
//! A local-delegate plan is executable template state, not declaration state. When its access is
//! copied across an inline boundary, the accessor bodies must follow that copy and use the same
//! split between static type substitution and runtime-reified substitution.

use std::collections::{HashMap, HashSet};

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
    // An expansion that substitutes nothing keeps the declaration's accessor plan. Cloning it
    // would publish a second plan for the same checked local property.
    if bindings.is_empty() && runtime.is_empty() {
        return Some(());
    }
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
    // The delegate operator is not itself reified. A copy whose accessor never names this
    // expansion's type arguments is the same helper; cloning it emits a second method with the
    // same JVM name.
    if !plan_mentions_bindings(ir, &plan, bindings, runtime, &mut HashSet::new()) {
        copies.insert(source_plan, source_plan);
        return Some(source_plan);
    }
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

fn plan_mentions_bindings(
    ir: &crate::ir::IrFile,
    plan: &crate::ir::IrLocalDelegatePlan,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
    seen: &mut HashSet<u32>,
) -> bool {
    type_mentions(plan.reference.property_type, bindings)
        || accessor_mentions(ir, &plan.getter, bindings, runtime, seen)
        || plan
            .setter
            .as_ref()
            .is_some_and(|accessor| accessor_mentions(ir, accessor, bindings, runtime, seen))
}

fn accessor_mentions(
    ir: &crate::ir::IrFile,
    accessor: &crate::ir::IrLocalDelegateAccessorPlan,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
    seen: &mut HashSet<u32>,
) -> bool {
    accessor
        .parameters
        .iter()
        .any(|ty| type_mentions(*ty, bindings))
        || type_mentions(accessor.result, bindings)
        || accessor.type_parameters.iter().any(|parameter| {
            parameter
                .bounds
                .iter()
                .any(|(bound, _)| type_mentions(*bound, bindings))
        })
        || body_mentions(ir, accessor.body, bindings, runtime, seen)
}

fn body_mentions(
    ir: &crate::ir::IrFile,
    root: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
    plans: &mut HashSet<u32>,
) -> bool {
    let mut pending = vec![root];
    let mut expressions = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !expressions.insert(expression) {
            continue;
        }
        if expression_mentions(ir, expression, bindings, runtime) {
            return true;
        }
        if let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) {
            if plans.insert(access.plan) {
                if let Some(plan) = ir.local_delegate_plans.get(access.plan as usize) {
                    if plan_mentions_bindings(ir, plan, bindings, runtime, plans) {
                        return true;
                    }
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

fn expression_mentions(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) -> bool {
    let direct_recorded = [
        ir.logical_types.get(&expression).copied(),
        ir.whens.exhaustive.get(&expression).copied(),
        ir.physical_types.get(&expression).copied(),
        ir.ext_call_source_receiver.get(&expression).copied(),
        ir.call_declared_ret.get(&expression).copied(),
        ir.suspend_calls.get(&expression).copied(),
    ]
    .into_iter()
    .flatten()
    .any(|ty| type_mentions(ty, bindings));
    if direct_recorded {
        return true;
    }
    let parameter_facts = [
        ir.call_declared_params.get(&expression),
        ir.construction_declared_params.get(&expression),
    ]
    .into_iter()
    .flatten()
    .flatten()
    .any(|ty| type_mentions(*ty, bindings));
    if parameter_facts {
        return true;
    }
    if ir
        .module_member_accesses
        .get(&expression)
        .is_some_and(|access| {
            let parameters = match access {
                crate::ir::IrModuleMemberAccess::Callable {
                    selected_parameters,
                    ..
                }
                | crate::ir::IrModuleMemberAccess::Property {
                    selected_parameters,
                    ..
                } => selected_parameters,
            };
            parameters.iter().any(|ty| type_mentions(*ty, bindings))
        })
    {
        return true;
    }
    if ir
        .reified_call_subst
        .get(&expression)
        .is_some_and(|substitutions| {
            substitutions
                .iter()
                .any(|(_, ty)| type_mentions(*ty, runtime))
        })
    {
        return true;
    }
    if ir
        .annotation_constructions
        .get(&expression)
        .is_some_and(|construction| {
            construction
                .members
                .iter()
                .any(|(_, ty)| type_mentions(*ty, bindings))
        })
    {
        return true;
    }
    if ir
        .value_class_suspend_calls
        .get(&expression)
        .is_some_and(|result| match result {
            crate::ir::IrValueClassSuspendResult::Boxed { carrier, .. }
            | crate::ir::IrValueClassSuspendResult::Carrier { carrier, .. } => {
                type_mentions(*carrier, bindings)
            }
        })
        || ir
            .intrinsic_suspension_points
            .get(&expression)
            .is_some_and(|point| type_mentions(point.result, bindings))
    {
        return true;
    }
    match ir.expr(expression) {
        IrExpr::Checked(operation) => checked_operation_mentions(operation, bindings),
        IrExpr::CallableReference(reference) => {
            let receiver = match &reference.target {
                crate::ir::IrCallableReferenceTarget::External { receiver, .. } => *receiver,
                _ => None,
            };
            receiver.is_some_and(|ty| type_mentions(ty, bindings))
                || type_mentions(reference.function_type, bindings)
                || reference
                    .declaration_parameters
                    .iter()
                    .any(|ty| type_mentions(*ty, bindings))
                || type_mentions(reference.declaration_result, bindings)
                || reference
                    .adaptation
                    .as_deref()
                    .is_some_and(|adaptation| adaptation_mentions(adaptation, bindings))
        }
        IrExpr::Call { callee, .. } => callee_mentions(callee, bindings, runtime),
        IrExpr::TypeOp { type_operand, .. } => type_mentions(*type_operand, runtime),
        IrExpr::KClassLiteral { classifier, .. } => {
            classifier.is_some_and(|ty| type_mentions(ty, runtime))
        }
        IrExpr::LocalPropertyReference(reference) => {
            type_mentions(reference.property_type, bindings)
        }
        IrExpr::Variable { ty, .. }
        | IrExpr::PrimitiveNeg { ty, .. }
        | IrExpr::PropertyRead { ty, .. }
        | IrExpr::PropertyWrite { ty, .. }
        | IrExpr::RefNew { elem: ty, .. }
        | IrExpr::RefGet { elem: ty, .. }
        | IrExpr::RefSet { elem: ty, .. }
        | IrExpr::Vararg { array_type: ty, .. }
        | IrExpr::NewArray { array_type: ty, .. }
        | IrExpr::Try { result: ty, .. } => type_mentions(*ty, bindings),
        IrExpr::New {
            ctor_params: Some(parameters),
            ..
        }
        | IrExpr::PluginPlaceholder {
            types: parameters, ..
        } => parameters.iter().any(|ty| type_mentions(*ty, bindings)),
        IrExpr::InvokeFunction { params, ret, .. } => {
            params.iter().any(|ty| type_mentions(*ty, bindings)) || type_mentions(*ret, bindings)
        }
        IrExpr::Lambda { sam: Some(sam), .. } => {
            sam.parameters
                .iter()
                .chain(&sam.declared_parameters)
                .any(|ty| type_mentions(*ty, bindings))
                || type_mentions(sam.result, bindings)
                || type_mentions(sam.declared_result, bindings)
        }
        _ => false,
    }
}

fn checked_operation_mentions(
    operation: &crate::ir::IrCheckedOperation,
    bindings: &HashMap<String, Ty>,
) -> bool {
    use crate::ir::IrCheckedOperation;

    match operation {
        IrCheckedOperation::Call {
            arguments,
            substitutions,
            ..
        }
        | IrCheckedOperation::ConstructorDelegation {
            arguments,
            substitutions,
            ..
        } => {
            arguments.iter().any(|argument| match argument {
                crate::ir::IrCheckedArgument::Vararg { array_type, .. } => {
                    type_mentions(*array_type, bindings)
                }
                _ => false,
            }) || substitutions
                .iter()
                .any(|substitution| substitution_mentions(substitution, bindings))
                || match operation {
                    IrCheckedOperation::ConstructorDelegation {
                        target,
                        outer_parameter,
                        ..
                    } => {
                        outer_parameter.is_some_and(|ty| type_mentions(ty, bindings))
                            || matches!(target, crate::ir::IrCheckedConstructorTarget::External { parameters, .. }
                                if parameters.iter().any(|ty| type_mentions(*ty, bindings)))
                    }
                    _ => false,
                }
        }
        IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => substitutions
            .iter()
            .any(|substitution| substitution_mentions(substitution, bindings)),
        IrCheckedOperation::ExternalPropertyRead {
            parameters,
            result,
            source_receiver,
            ..
        }
        | IrCheckedOperation::ExternalPropertyWrite {
            parameters,
            result,
            source_receiver,
            ..
        } => {
            parameters.iter().any(|ty| type_mentions(*ty, bindings))
                || type_mentions(*result, bindings)
                || source_receiver.is_some_and(|ty| type_mentions(ty, bindings))
        }
        IrCheckedOperation::RangeConstruction {
            start_type,
            end_type,
            result,
            ..
        } => [*start_type, *end_type, *result]
            .into_iter()
            .any(|ty| type_mentions(ty, bindings)),
        IrCheckedOperation::RangeContains { counter, .. }
        | IrCheckedOperation::RangeLoop { counter, .. } => type_mentions(*counter, bindings),
        IrCheckedOperation::PropertyReference {
            target,
            substitutions,
            adaptation,
            ..
        } => {
            property_reference_target_mentions(target, bindings)
                || substitutions
                    .iter()
                    .any(|substitution| substitution_mentions(substitution, bindings))
                || adaptation
                    .as_deref()
                    .is_some_and(|adaptation| adaptation_mentions(adaptation, bindings))
        }
        IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. }
        | IrCheckedOperation::IllegalProgressionStep { .. } => false,
    }
}

fn property_reference_target_mentions(
    target: &crate::fir::FirPropertyReferenceTarget,
    bindings: &HashMap<String, Ty>,
) -> bool {
    use crate::fir::FirPropertyReferenceTarget;

    match target {
        FirPropertyReferenceTarget::Module(_) => false,
        FirPropertyReferenceTarget::SpecializedModule {
            receiver,
            property_type,
            ..
        } => {
            receiver.is_some_and(|ty| type_mentions(ty.get(), bindings))
                || type_mentions(property_type.get(), bindings)
        }
        FirPropertyReferenceTarget::Classifier { property_type, .. } => {
            type_mentions(property_type.get(), bindings)
        }
        FirPropertyReferenceTarget::External {
            reflection_owner,
            getter,
            setter,
            property_type,
            ..
        } => {
            reflection_owner.is_some_and(|ty| type_mentions(ty.get(), bindings))
                || property_target_mentions(getter, bindings)
                || setter
                    .as_deref()
                    .is_some_and(|setter| property_target_mentions(setter, bindings))
                || type_mentions(property_type.get(), bindings)
        }
    }
}

fn property_target_mentions(
    target: &crate::fir::FirPropertyTarget,
    bindings: &HashMap<String, Ty>,
) -> bool {
    match target {
        crate::fir::FirPropertyTarget::Module { .. } => false,
        crate::fir::FirPropertyTarget::External {
            receiver,
            parameters,
            result,
            ..
        } => {
            receiver.is_some_and(|ty| type_mentions(ty.get(), bindings))
                || parameters
                    .iter()
                    .any(|ty| type_mentions(ty.get(), bindings))
                || type_mentions(result.get(), bindings)
        }
    }
}

fn adaptation_mentions(
    adaptation: &crate::fir::FirReferenceAdaptation,
    bindings: &HashMap<String, Ty>,
) -> bool {
    adaptation
        .parameter_types
        .iter()
        .any(|ty| type_mentions(ty.get(), bindings))
        || type_mentions(adaptation.result_type.get(), bindings)
}

fn callee_mentions(
    callee: &crate::ir::Callee,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) -> bool {
    use crate::ir::Callee;

    match callee {
        Callee::Intrinsic { operation, ret } => {
            intrinsic_mentions(*operation, runtime) || type_mentions(*ret, bindings)
        }
        Callee::CrossFile { params, ret, .. }
        | Callee::Module { params, ret, .. }
        | Callee::Super { params, ret, .. } => {
            params.iter().any(|ty| type_mentions(*ty, bindings)) || type_mentions(*ret, bindings)
        }
        Callee::ModuleWithDefaults {
            params,
            ret,
            dispatch_receiver_ty,
            ..
        } => {
            params.iter().any(|ty| type_mentions(*ty, bindings))
                || type_mentions(*ret, bindings)
                || dispatch_receiver_ty.is_some_and(|ty| type_mentions(ty, bindings))
        }
        Callee::External {
            params,
            ret,
            substitutions,
            ..
        } => {
            params.iter().any(|ty| type_mentions(*ty, bindings))
                || type_mentions(*ret, bindings)
                || substitutions
                    .iter()
                    .any(|substitution| substitution_mentions(substitution, runtime))
        }
        Callee::Virtual {
            params: Some((params, ret)),
            ..
        } => params.iter().any(|ty| type_mentions(*ty, bindings)) || type_mentions(*ret, bindings),
        Callee::Local(_)
        | Callee::ClassStatic { .. }
        | Callee::ClassStaticWithDefaults { .. }
        | Callee::ClassStaticDefault { .. }
        | Callee::LocalDefault(_)
        | Callee::LocalWithDefaults { .. }
        | Callee::Static { .. }
        | Callee::Virtual { params: None, .. }
        | Callee::Special { .. } => false,
    }
}

fn intrinsic_mentions(operation: crate::ir::IrIntrinsic, bindings: &HashMap<String, Ty>) -> bool {
    use crate::ir::IrIntrinsic;

    match operation {
        IrIntrinsic::EnumValueOf { classifier }
        | IrIntrinsic::TypeOf { ty: classifier }
        | IrIntrinsic::PrimitiveCompare {
            operand: classifier,
            ..
        }
        | IrIntrinsic::UnsignedToString { source: classifier }
        | IrIntrinsic::PrimitiveArrayNew {
            element: classifier,
        }
        | IrIntrinsic::Ieee754Equals {
            operand: classifier,
        }
        | IrIntrinsic::GeneratedPropertyEquals { ty: classifier }
        | IrIntrinsic::GeneratedPropertyHash { ty: classifier }
        | IrIntrinsic::DataClassArrayToString { ty: classifier } => {
            type_mentions(classifier, bindings)
        }
        IrIntrinsic::Assert { .. }
        | IrIntrinsic::ArrayGet
        | IrIntrinsic::ArraySet
        | IrIntrinsic::ArraySize
        | IrIntrinsic::StringGet
        | IrIntrinsic::StringLength
        | IrIntrinsic::EnumName
        | IrIntrinsic::NullableAnyToString
        | IrIntrinsic::CoroutineContext => false,
    }
}

fn substitution_mentions(
    substitution: &crate::ir::IrCheckedSubstitution,
    bindings: &HashMap<String, Ty>,
) -> bool {
    type_mentions(substitution.value, bindings)
        || substitution
            .additional_bounds
            .iter()
            .any(|ty| type_mentions(*ty, bindings))
}

fn type_mentions(ty: Ty, bindings: &HashMap<String, Ty>) -> bool {
    !bindings.is_empty() && crate::types::ty_subst_keep_unbound(ty, bindings) != ty
}
