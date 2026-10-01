//! JVM type-safe collection barriers selected from resolved override edges.
//!
//! Common IR retains the overridden declaration identity and semantic signature. This pass records
//! one plan per bridge and one plan per same-descriptor method entry. Emission writes those plans
//! and does not select them again.

use crate::ir::{Bridge, BridgeKind, CollectionBarrierPlan, FunId, IrFile, IrFunctionOverride};
use crate::types::Ty;

pub(crate) use crate::libraries::CollectionBarrierOutcome as BarrierOutcome;

/// JVM method-entry guards for collection overrides whose erased descriptor needs no bridge.
#[derive(Default)]
pub(crate) struct MethodEntryBarriers(std::collections::HashMap<FunId, CollectionBarrierPlan>);

impl MethodEntryBarriers {
    pub(crate) fn plan(&self, function: FunId) -> Option<CollectionBarrierPlan> {
        self.0.get(&function).copied()
    }
}

fn valid_result(outcome: BarrierOutcome, erased: Ty, concrete: Ty) -> bool {
    match outcome {
        BarrierOutcome::False => erased == Ty::Boolean && concrete == Ty::Boolean,
        BarrierOutcome::NotFound => erased == Ty::Int && concrete == Ty::Int,
        BarrierOutcome::Null => erased.is_reference() && concrete.is_reference(),
    }
}

fn bridge_semantics(bridge: &Bridge) -> Option<CollectionBarrierPlan> {
    if bridge.kind != BridgeKind::Function {
        return None;
    }
    let outcome = bridge.collection_barrier?;
    if !valid_result(outcome, bridge.erased_ret, bridge.concrete_ret) {
        return None;
    }
    let parameter = 0;
    (bridge.erased_params.len() == 1
        && bridge.concrete_params.len() == 1
        && bridge.erased_params[parameter].is_erased_top()
        && narrow_parameter(bridge.concrete_params[parameter]))
    .then_some(CollectionBarrierPlan { parameter, outcome })
}

/// A collection parameter that is narrower than the erased `Object` slot. `Nothing` occupies the
/// `java/lang/Void` slot even though it has no inhabitable reference value.
fn narrow_parameter(ty: Ty) -> bool {
    !ty.is_erased_top()
        && (signed_jvm_primitive(ty) || ty.is_reference() || ty.non_null() == Ty::Nothing)
}

fn signed_jvm_primitive(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Boolean | Ty::Byte | Ty::Short | Ty::Int | Ty::Long | Ty::Char | Ty::Float | Ty::Double
    )
}

fn method_entry_semantics(
    edge: &IrFunctionOverride,
    physical_result: Ty,
) -> Option<CollectionBarrierPlan> {
    let outcome = edge.collection_barrier?;
    let [parameter] = edge.implementation_parameters.as_slice() else {
        return None;
    };
    (*parameter == Ty::obj("kotlin/Any") && edge.declared_parameters.len() == 1)
        .then_some(CollectionBarrierPlan {
            parameter: 0,
            outcome,
        })
        .filter(|plan| valid_result(plan.outcome, edge.applied_result, physical_result))
}

pub(crate) fn select(
    ir: &mut IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    method_entries: &mut MethodEntryBarriers,
) {
    method_entries.0.clear();
    for (&class, edges) in &ir.function_overrides {
        for edge in edges {
            if edge.implementation_owner != class {
                continue;
            }
            let Some(function) = crate::jvm::bridges::implementation_function(ir, edge) else {
                continue;
            };
            let physical_result = override_results.physical_result(ir, function);
            let Some(plan) = method_entry_semantics(edge, physical_result) else {
                continue;
            };
            method_entries.0.insert(function, plan);
        }
    }
    for class in &mut ir.classes {
        for bridge in &mut class.bridges {
            let barrier = bridge_semantics(bridge);
            bridge.barrier_plan = barrier;
            crate::trace_compiler!(
                "lower",
                "collection bridge class={} name={} overridden_owner={:?} barrier={:?}",
                class.fq_name,
                bridge.name,
                bridge.overridden_owner,
                barrier,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bridge_semantics, BarrierOutcome};
    use crate::types::Ty;

    fn bridge(
        role: Option<BarrierOutcome>,
        param: Ty,
        erased_ret: Ty,
        concrete_ret: Ty,
    ) -> crate::ir::Bridge {
        crate::ir::Bridge {
            kind: crate::ir::BridgeKind::Function,
            target_function: None,
            overridden_owner: Some(crate::types::type_name("example/Owner")),
            collection_barrier: role,
            parameters: Vec::new(),
            name: "spelling_is_not_identity".to_string(),
            erased_params: vec![Ty::obj("kotlin/Any")],
            erased_ret,
            concrete_params: vec![param],
            concrete_ret,
            target_ret: None,
            barrier_plan: None,
            special: false,
            target_name: None,
        }
    }

    #[test]
    fn narrow_collection_parameters_take_the_type_safe_barrier() {
        let boolean = Ty::Boolean;
        let any = Ty::obj("kotlin/Any");
        let string = Ty::obj("kotlin/String");
        assert_eq!(
            bridge_semantics(&bridge(
                Some(BarrierOutcome::False),
                Ty::Int,
                boolean,
                boolean
            ))
            .map(|barrier| barrier.outcome),
            Some(BarrierOutcome::False)
        );
        assert_eq!(
            bridge_semantics(&bridge(
                Some(BarrierOutcome::False),
                string,
                boolean,
                boolean,
            ))
            .map(|barrier| barrier.outcome),
            Some(BarrierOutcome::False)
        );
        assert!(
            bridge_semantics(&bridge(Some(BarrierOutcome::False), any, boolean, boolean,))
                .is_none()
        );
        assert!(bridge_semantics(&bridge(None, Ty::Int, boolean, boolean)).is_none());
        assert_eq!(
            bridge_semantics(&bridge(
                Some(BarrierOutcome::NotFound),
                Ty::Nothing,
                Ty::Int,
                Ty::Int,
            ))
            .map(|barrier| (barrier.parameter, barrier.outcome)),
            Some((0, BarrierOutcome::NotFound))
        );
        assert_eq!(
            bridge_semantics(&bridge(Some(BarrierOutcome::Null), Ty::Int, any, string,))
                .map(|barrier| barrier.outcome),
            Some(BarrierOutcome::Null)
        );
    }
}
