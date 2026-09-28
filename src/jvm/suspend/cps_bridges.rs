//! Suspend override bridges realized over the CPS signature their targets now have.

use super::{continuation_ty, object_ty};
use crate::ir::IrFile;
use crate::types::TypeName;
use std::collections::HashSet;

/// Convert already-derived suspend override bridges from their declared Kotlin signature to the JVM
/// CPS signature. Bridge derivation intentionally runs before value-class realization, so that pass can
/// reuse its normal target mangling and argument/return adaptation. Once concrete suspend methods have
/// gained their trailing continuation, both sides of each matching bridge gain the same parameter and
/// return `Object`; any pre-CPS result boxing is removed because the concrete suspend method already
/// crosses the continuation boundary in boxed form.
///
/// Runs once [`super::lower_suspend`] has given the concrete suspend methods their CPS signatures.
pub(crate) fn finalize_suspend_bridges(
    ir: &mut IrFile,
    adaptations: &mut crate::jvm::bridge_adaptations::BridgeAdaptations,
) {
    let continuation = continuation_ty();
    let object = object_ty();
    let suspend_targets = method_keys(ir, |fid| ir.suspend_funs.contains(&fid));
    let boxed_suspend_targets = method_keys(ir, |fid| {
        matches!(
            ir.value_class_suspend_returns.get(&fid),
            Some(crate::ir::IrValueClassSuspendResult::Boxed { .. })
        )
    });
    for class in &mut ir.classes {
        let owner = class.fq_name_id();
        for (ordinal, bridge) in class.bridges.iter_mut().enumerate() {
            let ordinal = u32::try_from(ordinal).expect("bridge ordinals fit u32");
            let target = bridge.target_name.as_deref().unwrap_or(&bridge.name);
            if !suspend_targets.contains(&(
                class.fq_name,
                target.to_string(),
                bridge.concrete_params.len(),
            )) {
                continue;
            }
            bridge.erased_params.push(continuation);
            bridge.concrete_params.push(continuation);
            bridge
                .parameter_identities
                .push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
            bridge.parameter_types.push(continuation);
            bridge.erased_ret = object;
            let target_key = (
                class.fq_name,
                target.to_string(),
                bridge.concrete_params.len().saturating_sub(1),
            );
            let target_returns_boxed = boxed_suspend_targets.contains(&target_key);
            if adaptations.boxes_result(owner, ordinal)
                && bridge.concrete_ret.is_reference()
                && !target_returns_boxed
            {
                // A generic supertype boundary needs a BOX, while a reference-carrier suspend target
                // returns the raw carrier as Object. Preserve the pre-CPS carrier adaptation and record
                // only the target descriptor's erased return separately.
                bridge.target_ret = Some(object);
            } else {
                // A scalar-carrier suspend target already boxed its value class before `areturn`; a
                // non-value-class bridge needs no result adaptation either. Forward Object directly.
                bridge.concrete_ret = object;
                bridge.target_ret = None;
                adaptations.remove_result_box(owner, ordinal);
            }
            crate::trace_compiler!(
                "bridges",
                "finalized suspend bridge {}::{} -> {} arity={} target_boxed={target_returns_boxed}",
                class.fq_name,
                bridge.name,
                target,
                bridge.erased_params.len()
            );
        }
    }
}

/// The class methods `selected` answers for, by owner, name and declared arity (without the
/// continuation their CPS signature appends).
fn method_keys(ir: &IrFile, selected: impl Fn(u32) -> bool) -> HashSet<(TypeName, String, usize)> {
    ir.classes
        .iter()
        .flat_map(|class| {
            class
                .methods
                .iter()
                .copied()
                .filter(|&fid| selected(fid))
                .map(|fid| {
                    let function = &ir.functions[fid as usize];
                    (
                        class.fq_name,
                        function.name.clone(),
                        function.params.len().saturating_sub(1),
                    )
                })
        })
        .collect()
}
