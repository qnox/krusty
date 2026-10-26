//! JVM type-safe collection barriers selected from resolved override edges.
//!
//! Common IR retains the overridden declaration identity and semantic signature. This pass records
//! one plan per bridge and one plan per same-descriptor method entry. Emission writes those plans
//! and does not select them again.

use crate::ir::{
    Bridge, BridgeKind, CollectionBarrierCheck, CollectionBarrierParameter, CollectionBarrierPlan,
    FunId, IrFile, IrFunctionOverride,
};
use crate::libraries::CollectionBarrier;
use crate::types::Ty;

pub(crate) use crate::libraries::CollectionBarrierOutcome as BarrierOutcome;

/// JVM method-entry guards for collection overrides whose erased descriptor needs no bridge.
#[derive(Default)]
pub(crate) struct MethodEntryBarriers(crate::fx_hash::HashMap<FunId, CollectionBarrierPlan>);

impl MethodEntryBarriers {
    pub(crate) fn plan(&self, function: FunId) -> Option<&CollectionBarrierPlan> {
        self.0.get(&function)
    }
}

fn valid_result(outcome: BarrierOutcome, erased: Ty, concrete: Ty) -> bool {
    match outcome {
        BarrierOutcome::False => erased == Ty::Boolean && concrete == Ty::Boolean,
        BarrierOutcome::NotFound => erased == Ty::Int && concrete == Ty::Int,
        BarrierOutcome::Null => erased.is_reference() && concrete.is_reference(),
    }
}

/// Whether the override's semantic type for a parameter accepts `null`: `T?`, or a type parameter
/// whose bound does (`V` under `Any?`).
fn admits_null(semantic: Ty) -> bool {
    semantic.admits_null() || semantic.upper_bound_admits_null()
}

/// The override's semantic parameter types, with an enclosing class's type parameters intact.
fn semantic_parameters(ir: &IrFile, function: FunId) -> Vec<Ty> {
    ir.member_semantic_sigs.get(&function).map_or_else(
        || ir.functions[function as usize].params.clone(),
        |(params, _)| params.clone(),
    )
}

/// kotlinc's special bridge tests every checked parameter (`p is T`) once any of them is narrower
/// than its erased `Object` slot: `instanceof` for a narrower one, a presence test for one that
/// erases to `Object`.
fn bridge_semantics(bridge: &Bridge, semantic: &[Ty]) -> Option<CollectionBarrierPlan> {
    if bridge.kind != BridgeKind::Function {
        return None;
    }
    let CollectionBarrier {
        checked_parameters,
        outcome,
    } = bridge.collection_barrier?;
    if !valid_result(outcome, bridge.erased_ret, bridge.concrete_ret)
        || bridge.erased_params.len() != bridge.concrete_params.len()
        || semantic.len() != bridge.concrete_params.len()
    {
        return None;
    }
    let checked: Vec<usize> = checked_parameters
        .iter()
        .map(|&index| usize::from(index))
        .collect();
    if checked.iter().any(|&index| {
        index >= bridge.erased_params.len() || !bridge.erased_params[index].is_erased_top()
    }) || !checked
        .iter()
        .any(|&index| narrow_parameter(bridge.concrete_params[index]))
    {
        return None;
    }
    let parameters = checked
        .into_iter()
        .map(|parameter| {
            let admits_null = admits_null(semantic[parameter]);
            let check = if narrow_parameter(bridge.concrete_params[parameter]) {
                CollectionBarrierCheck::Instance { admits_null }
            } else {
                CollectionBarrierCheck::Presence { admits_null }
            };
            CollectionBarrierParameter { parameter, check }
        })
        .collect();
    Some(CollectionBarrierPlan {
        parameters,
        outcome,
    })
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

/// An override that keeps the erased descriptor gets no bridge, so its own entry rejects a `null`
/// argument for each checked parameter whose type erases to `Object` but excludes `null`, and
/// returns the neutral result instead of failing a parameter assertion.
fn method_entry_semantics(
    edge: &IrFunctionOverride,
    parameters: &[Ty],
    semantic: &[Ty],
    physical_result: Ty,
) -> Option<CollectionBarrierPlan> {
    let CollectionBarrier {
        checked_parameters,
        outcome,
    } = edge.collection_barrier?;
    if parameters.len() != edge.declared_parameters.len()
        || semantic.len() != parameters.len()
        || !parameters.iter().all(|parameter| parameter.is_erased_top())
        || !valid_result(outcome, edge.applied_result, physical_result)
    {
        return None;
    }
    let guarded: Vec<CollectionBarrierParameter> = checked_parameters
        .iter()
        .map(|&index| usize::from(index))
        .filter(|&parameter| parameter < semantic.len() && !admits_null(semantic[parameter]))
        .map(|parameter| CollectionBarrierParameter {
            parameter,
            check: CollectionBarrierCheck::Presence { admits_null: false },
        })
        .collect();
    (!guarded.is_empty()).then_some(CollectionBarrierPlan {
        parameters: guarded,
        outcome,
    })
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
            let semantic = semantic_parameters(ir, function);
            let parameters = &ir.functions[function as usize].params;
            let Some(plan) = method_entry_semantics(edge, parameters, &semantic, physical_result)
            else {
                continue;
            };
            method_entries.0.insert(function, plan);
        }
    }
    let plans: Vec<Vec<Option<CollectionBarrierPlan>>> = ir
        .classes
        .iter()
        .map(|class| {
            class
                .bridges
                .iter()
                .map(|bridge| {
                    let semantic = bridge.target_function.map_or_else(
                        || bridge.concrete_params.clone(),
                        |function| semantic_parameters(ir, function),
                    );
                    bridge_semantics(bridge, &semantic)
                })
                .collect()
        })
        .collect();
    for (class, plans) in ir.classes.iter_mut().zip(plans) {
        for (bridge, barrier) in class.bridges.iter_mut().zip(plans) {
            bridge.barrier_plan = barrier;
            crate::trace_compiler!(
                "lower",
                "collection bridge class={} name={} overridden_owner={:?} barrier={:?}",
                class.fq_name,
                bridge.name,
                bridge.overridden_owner,
                bridge.barrier_plan,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bridge_semantics, BarrierOutcome};
    use crate::ir::{CollectionBarrierCheck, CollectionBarrierParameter};
    use crate::libraries::CollectionBarrier;
    use crate::types::Ty;

    fn bridge(
        outcome: Option<BarrierOutcome>,
        params: &[Ty],
        erased_ret: Ty,
        concrete_ret: Ty,
    ) -> crate::ir::Bridge {
        const CHECKED: [u16; 2] = [0, 1];
        crate::ir::Bridge {
            kind: crate::ir::BridgeKind::Function,
            target_function: None,
            overridden_owner: Some(crate::types::type_name("example/Owner")),
            collection_barrier: outcome.map(|outcome| CollectionBarrier {
                checked_parameters: &CHECKED[..params.len()],
                outcome,
            }),
            parameters: Vec::new(),
            name: "spelling_is_not_identity".to_string(),
            erased_params: vec![Ty::obj("kotlin/Any"); params.len()],
            erased_ret,
            concrete_params: params.to_vec(),
            concrete_ret,
            target_ret: None,
            barrier_plan: None,
            special: false,
            module_name_bridge: false,
            target_name: None,
            property_implementation: None,
        }
    }

    fn outcome(bridge: &crate::ir::Bridge) -> Option<BarrierOutcome> {
        bridge_semantics(bridge, &bridge.concrete_params).map(|barrier| barrier.outcome)
    }

    #[test]
    fn narrow_collection_parameters_take_the_type_safe_barrier() {
        let boolean = Ty::Boolean;
        let any = Ty::obj("kotlin/Any");
        let string = Ty::obj("kotlin/String");
        let barrier = Some(BarrierOutcome::False);
        assert_eq!(
            outcome(&bridge(barrier, &[Ty::Int], boolean, boolean)),
            barrier
        );
        assert_eq!(
            outcome(&bridge(barrier, &[string], boolean, boolean)),
            barrier
        );
        assert!(outcome(&bridge(barrier, &[any], boolean, boolean)).is_none());
        assert!(outcome(&bridge(None, &[Ty::Int], boolean, boolean)).is_none());
        let not_found = bridge(
            Some(BarrierOutcome::NotFound),
            &[Ty::Nothing],
            Ty::Int,
            Ty::Int,
        );
        assert_eq!(
            bridge_semantics(&not_found, &not_found.concrete_params).map(|plan| plan.parameters),
            Some(vec![CollectionBarrierParameter {
                parameter: 0,
                check: CollectionBarrierCheck::Instance { admits_null: false },
            }])
        );
        assert_eq!(
            outcome(&bridge(Some(BarrierOutcome::Null), &[Ty::Int], any, string)),
            Some(BarrierOutcome::Null)
        );
    }

    #[test]
    fn every_checked_parameter_is_tested_once_one_is_narrower() {
        let any = Ty::obj("kotlin/Any");
        let string = Ty::obj("kotlin/String");
        let checks = |params: &[Ty], semantic: &[Ty]| {
            let bridge = bridge(
                Some(BarrierOutcome::False),
                params,
                Ty::Boolean,
                Ty::Boolean,
            );
            bridge_semantics(&bridge, semantic).map(|plan| {
                plan.parameters
                    .into_iter()
                    .map(|parameter| parameter.check)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            checks(&[string, any], &[string, Ty::nullable(any)]),
            Some(vec![
                CollectionBarrierCheck::Instance { admits_null: false },
                CollectionBarrierCheck::Presence { admits_null: true },
            ])
        );
        assert_eq!(
            checks(&[any, Ty::nullable(string)], &[any, Ty::nullable(string)]),
            Some(vec![
                CollectionBarrierCheck::Presence { admits_null: false },
                CollectionBarrierCheck::Instance { admits_null: true },
            ])
        );
        assert_eq!(checks(&[any, any], &[any, any]), None);
    }
}
