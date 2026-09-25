//! The instance entries through which a boxed value class implements its interfaces.
//!
//! A value class's own members are realized as static functions over its carrier
//! (`f-<hash>(carrier, params)`), but an interface call dispatches on the boxed object. kotlinc
//! therefore gives the box one ordinary public instance method per overriding member, with the
//! member's own physical name and parameters: it checks its parameters as the member does, reads
//! the carrier and calls the static function. Every descriptor-changing bridge the member needs
//! (a generic `f(Object)`) calls that entry, not the static function.

use super::*;

/// Add each value class's interface entries as bridges targeting the static member, and return the
/// static members that have one: another bridge to such a member calls its entry instead.
pub(super) fn materialize(
    ir: &mut IrFile,
    lowered_value_members: &HashSet<u32>,
    entry_name: impl Fn(&IrFile, u32) -> String,
) -> HashSet<u32> {
    let mut entries = Vec::new();
    for (&owner, edges) in &ir.function_overrides {
        let Some(class) = ir.classes.iter().position(|class| class.fq_name == owner) else {
            continue;
        };
        if !ir.classes[class].is_value {
            continue;
        }
        for edge in edges {
            if edge.implementation_owner != owner || !edge.overridden_is_interface {
                continue;
            }
            let implementation = edge.implementation_function.or_else(|| {
                let crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) =
                    edge.implementation
                else {
                    return None;
                };
                ir.checked_callable_functions.get(&declaration).copied()
            });
            let Some(implementation) = implementation.filter(|function| {
                lowered_value_members.contains(function)
                    && ir.classes[class].methods.contains(function)
            }) else {
                continue;
            };
            let entry = (
                class,
                implementation,
                edge.implementation_parameter_identities.clone(),
            );
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    let mut targets = HashSet::new();
    for (class, implementation, parameter_identities) in entries {
        // The static member's physical signature, less the carrier it receives first.
        let target = &ir.functions[implementation as usize];
        let parameters = target
            .params
            .get(1..)
            .expect("a static value-class member carries its receiver")
            .to_vec();
        assert_eq!(
            parameter_identities.len(),
            parameters.len(),
            "a value-class interface entry retains its semantic parameter identities"
        );
        let result = target.ret;
        let name = entry_name(ir, implementation);
        let bridges = &mut ir.classes[class].bridges;
        if !bridges.iter().any(|bridge| {
            bridge.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry
                && bridge.target_function == Some(implementation)
        }) {
            bridges.push(crate::ir::Bridge {
                kind: crate::ir::BridgeKind::ValueClassInterfaceEntry,
                target_function: Some(implementation),
                parameter_identities,
                name,
                erased_params: parameters.clone(),
                erased_ret: result,
                concrete_params: parameters,
                concrete_ret: result,
                target_ret: None,
                type_safe_barrier: false,
                target_name: None,
                box_ret: None,
                unbox_params: Vec::new(),
            });
        }
        targets.insert(implementation);
    }
    targets
}
