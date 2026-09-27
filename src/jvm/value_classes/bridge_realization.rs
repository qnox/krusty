//! Bridges retargeted onto the value-class carriers their targets now take and return.
//!
//! A bridge was derived from the Kotlin declarations before value classes were erased. Once the
//! pass has mangled and erased the targets, each bridge names its target's physical spelling, and
//! what crosses its boundary boxed is recorded as its JVM adapter plan.

use super::*;
use crate::jvm::bridge_adaptations::{
    BridgeAdaptations, BridgeAdapter, BridgeResultAdapter, ValueClassAdapter,
};

/// What the value-class pass has decided by the time it realizes bridges.
pub(super) struct Inputs<'a> {
    pub(super) under: &'a Under,
    /// `under` plus the value classes that only a callable signature names.
    pub(super) callable_under: &'a Under,
    /// Each mangled member by `(owner, source name, arity)`.
    pub(super) mangle_map: &'a HashMap<(TypeName, String, usize), String>,
    pub(super) suspend_sig: &'a HashSet<(Option<TypeName>, String, usize)>,
    /// Each value-class member rewritten to a static carrier function, by its stable identity:
    /// the physical name, parameters and result it now has.
    pub(super) lowered_member_targets: &'a HashMap<u32, (String, Vec<Ty>, Ty)>,
    /// The value-class members an interface entry now stands for.
    pub(super) interface_entries: &'a HashSet<u32>,
}

/// Retarget every bridge of `classes` onto its realized target and record its adapter plan.
pub(super) fn realize(
    classes: &mut [crate::ir::IrClass],
    inputs: &Inputs<'_>,
    bridge_adaptations: &mut BridgeAdaptations,
) {
    let Inputs {
        under,
        callable_under,
        mangle_map,
        suspend_sig,
        lowered_member_targets,
        interface_entries,
    } = *inputs;
    // Adapter plans, collected while `ir.classes` is borrowed and published after.
    let mut adapters = Vec::new();
    for c in classes.iter_mut() {
        let owner_is_value = c.is_value;
        let owner_fq = c.fq_name();
        let owner_fq_id = c.fq_name_id();
        let naming = bridge_names::BridgeNaming {
            owner: c.fq_name,
            under: callable_under,
            suspend: suspend_sig,
        };
        for (bridge_index, b) in c.bridges.iter_mut().enumerate() {
            let bridge_index = bridge_index as u32;
            let mut adapter = BridgeAdapter::default();
            let lowered_member_target = b
                .target_function
                .and_then(|function| lowered_member_targets.get(&function))
                .cloned();
            let logical_concrete_params = b.concrete_params.clone();
            let target = b.target_name.clone().unwrap_or_else(|| b.name.clone());
            if let Some(m) = mangle_map.get(&(c.fq_name, target.clone(), b.concrete_params.len())) {
                b.target_name = Some(m.clone());
            }
            let target_mentions_vc = bridge_returns::mentions_value_class(
                &b.concrete_params,
                b.concrete_ret,
                callable_under,
            );
            let bridge_mentions_vc = bridge_returns::mentions_value_class(
                &b.erased_params,
                b.erased_ret,
                callable_under,
            );
            if b.target_name.is_none()
                && target_mentions_vc
                && b.kind != crate::ir::BridgeKind::PropertyGetter
            {
                let mangled = vc_mangle(
                    &target,
                    &b.concrete_params,
                    &b.concrete_ret,
                    callable_under,
                    false,
                    suspend_sig.contains(&(
                        Some(c.fq_name),
                        target.clone(),
                        b.concrete_params.len(),
                    )),
                );
                if mangled != target {
                    b.target_name = Some(mangled);
                }
            }
            crate::trace_compiler!(
                "value_classes",
                "bridge {}::{} target={:?} concrete_ret={:?} erased_ret={:?}",
                owner_fq,
                b.name,
                b.target_name,
                b.concrete_ret,
                b.erased_ret
            );
            let concrete_ret_vc =
                bridge_parameters::carried_value_class(&b.concrete_ret, callable_under);
            let erased_ret_vc = b
                .erased_ret
                .non_null()
                .obj_internal()
                .filter(|fq_name| callable_under.contains_key(fq_name));
            if !owner_is_value && !bridge_mentions_vc {
                if let Some(parameters) = bridge_parameters::unbox_arguments(b, callable_under) {
                    adapter.parameters = parameters;
                }
            }
            if let Some(fq_name) = concrete_ret_vc {
                if b.target_name.is_none() {
                    b.target_name = Some(b.name.clone());
                }
                // The bridge satisfies the (mangled) SUPERTYPE method, so it takes that method's
                // mangled name: `vc_mangle` over the override's params + the SUPERTYPE's declared
                // return. A VC param (`foo(i: Marker)`) mangles by the param; a literal-VC return
                // (`fun bar(): Gx`) also mangles by the return; a generic `T` return (erased
                // `Object`) does not.
                // A bridge lives on a class (never a file class); its value-class return mangles.
                if bridge_mentions_vc {
                    naming.mangle_by_override(b);
                }
                // A value-class PARAM erases to its underlying in both the bridge descriptor and the
                // target call (`foo-<hash>(Marker)` → `foo-<hash>(int)`). Done AFTER the mangle,
                // which keys on the un-erased param type.
                for p in b
                    .erased_params
                    .iter_mut()
                    .chain(b.concrete_params.iter_mut())
                {
                    *p = erase(p, callable_under);
                }
                // Whether the SUPERTYPE method returns the value class in its UNBOXED form — a non-null
                // literal (`fun bar(): Gx`), OR a nullable `X?` whose underlying is a non-null reference
                // (so `X?` stays UNBOXED, carrying null itself, e.g. `X(val x: Any)`). Then the bridge
                // returns the erased underlying, NO box. A nullable `X?` that BOXES (over a primitive /
                // null-capable chain, e.g. `X(val x: Any?)` → `LX;`) or a generic `T` (erased `Object`)
                // → bridge BOXES the value class back.
                let supertype_returns_vc =
                    b.erased_ret
                        .non_null()
                        .obj_internal()
                        .is_some_and(|fq_name| {
                            callable_under.contains_key(&fq_name)
                                && (!b.erased_ret.is_nullable()
                                    || !nullable_is_boxed(fq_name, callable_under))
                        });
                let concrete_carrier = erase(&b.concrete_ret, callable_under);
                // An EXTERNAL value class (`Result`) is held unboxed (`Object`) everywhere in krusty —
                // when the SUPERTYPE also carries it unboxed the bridge returns the override's already-
                // `Object` result directly, NO `box-impl`. EXCEPTION: a GENERIC boundary — the supertype
                // method returns an erased type variable (`fun performOperation(): T` → `Object`). There
                // kotlinc materializes the box (`Result.box-impl(Object)Lkotlin/Result;`) so the caller
                // observes the boxed object (its `toString`/identity), and krusty must match: the
                // bridge boxes through the classpath `box-impl`, exactly like a user value class.
                if supertype_returns_vc {
                    b.concrete_ret = concrete_carrier;
                    b.erased_ret = b.concrete_ret;
                } else {
                    adapter.result = Some(BridgeResultAdapter::Box(ValueClassAdapter::new(
                        fq_name,
                        b.concrete_ret.is_nullable(),
                    )));
                    b.concrete_ret = concrete_carrier;
                }
            } else if erased_ret_vc.is_some() {
                // A bottom/null override (`Nothing`/`Nothing?`) can implement a value-class-returning
                // member. The concrete target is not itself a value-class return, but the bridge still
                // satisfies the SUPERTYPE declaration, whose JVM name is mangled by its value-class
                // return type (`foo(): X?` -> `foo-<hash>()LX;`). Keep the target's source name and
                // publish the bridge under the mangled supertype name.
                if b.target_name.is_none() {
                    b.target_name = Some(b.name.clone());
                }
                naming.mangle_by_override(b);
                for p in b.erased_params.iter_mut() {
                    *p = erase(p, callable_under);
                }
                if let Some(plan) = bridge_returns::plan_unboxing(b, erased_ret_vc, callable_under)
                {
                    adapter.result = Some(BridgeResultAdapter::Unbox(plan));
                    b.erased_ret = erase(&b.erased_ret, callable_under);
                }
            } else if !owner_is_value && bridge_mentions_vc {
                // A bridge (mangled `f-<hash>` OR same-name) delegating to a concrete method with a
                // VALUE-CLASS PARAM, where the bridge's OWN param is the erased-generic `Object`: a
                // generic supertype method (`I<Result>.foo(T)`) keeps its `foo(Object)` bridge signature,
                // but the incoming arg is a BOXED `X` (the generic call site boxes). Record each such
                // param to `checkcast` + `unbox-impl`, then erase the concrete param to its underlying for
                // the delegated call. A param already AT its underlying (bridge param not a reference —
                // a primitive-underlying value class) needs no unbox.
                if let Some(parameters) = bridge_parameters::unbox_arguments(b, callable_under) {
                    adapter.parameters = parameters;
                }
                naming.answer_to_value_class_parameters(b, &target);
            }
            if let Some((target_name, target_params, target_ret)) = lowered_member_target {
                let physical_params = target_params
                    .get(1..)
                    .expect("a static value-class member target must carry its receiver");
                assert_eq!(
                    logical_concrete_params.len(),
                    physical_params.len(),
                    "a value-class bridge target must preserve source parameter arity"
                );
                adapter.parameters = logical_concrete_params
                    .iter()
                    .zip(&b.erased_params)
                    .zip(physical_params)
                    .map(|((logical, erased), physical)| {
                        logical
                            .non_null()
                            .obj_internal()
                            .filter(|classifier| {
                                under.contains_key(classifier)
                                    && is_ref(erased)
                                    && physical.non_null().obj_internal() != Some(*classifier)
                            })
                            .map(|classifier| {
                                ValueClassAdapter::new(classifier, logical.is_nullable())
                            })
                    })
                    .collect();
                b.concrete_params = physical_params.to_vec();
                b.target_name = Some(target_name);
                if target_ret != b.concrete_ret {
                    b.target_ret = Some(target_ret);
                }
                // Only the entry calls the static member; any other bridge calls the entry,
                // which has the member's physical name and parameters on the box itself.
                if b.kind != crate::ir::BridgeKind::ValueClassInterfaceEntry
                    && b.target_function
                        .is_some_and(|function| interface_entries.contains(&function))
                {
                    b.target_function = None;
                }
            }
            adapters.push(((owner_fq_id, bridge_index), adapter));
        }
    }
    bridge_adaptations.extend(adapters);
}
