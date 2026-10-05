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
/// static members that have one, with the entry's name: another bridge to such a member calls its
/// entry instead. A property accessor overriding an interface accessor is such a member exactly as
/// a function is (kotlinc's `JvmInlineClassLowering` replaces both through one path), so both kinds
/// of override edge contribute entries of the same shape.
pub(super) fn materialize(
    ir: &mut IrFile,
    static_members: &HashSet<u32>,
    override_results: &crate::jvm::override_results::OverrideResults,
    entry_name: impl Fn(&IrFile, u32) -> String,
) -> HashMap<u32, String> {
    let mut entries = Vec::new();
    for (class, declaration) in ir.classes.iter().enumerate() {
        if !declaration.is_value {
            continue;
        }
        let owner = declaration.fq_name;
        let candidates = function_entries(ir, owner).chain(accessor_entries(ir, owner));
        for (implementation, parameters) in candidates {
            if !static_members.contains(&implementation)
                || !declaration.methods.contains(&implementation)
            {
                continue;
            }
            let entry = (class, implementation, parameters);
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    let mut targets = HashMap::new();
    for (class, implementation, bridge_parameters) in entries {
        // The static member's physical signature, less the carrier it receives first.
        let target = &ir.functions[implementation as usize];
        let parameters = target
            .params
            .get(1..)
            .expect("a static value-class member carries its receiver")
            .to_vec();
        assert_eq!(
            bridge_parameters.len(),
            parameters.len(),
            "a value-class interface entry retains its semantic parameters"
        );
        // The member's JVM result: the wrapper where a primitive override result is boxed.
        let result = override_results.physical_result(ir, implementation);
        let name = entry_name(ir, implementation);
        let bridges = &mut ir.classes[class].bridges;
        if !bridges.iter().any(|bridge| {
            bridge.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry
                && bridge.target_function == Some(implementation)
        }) {
            bridges.push(crate::ir::Bridge {
                kind: crate::ir::BridgeKind::ValueClassInterfaceEntry,
                target_function: Some(implementation),
                overridden_owner: None,
                collection_barrier: None,
                parameters: bridge_parameters,
                name: name.clone(),
                erased_params: parameters.clone(),
                erased_ret: result,
                concrete_params: parameters,
                concrete_ret: result,
                target_ret: None,
                barrier_plan: None,
                special: false,
                module_name_bridge: false,
                target_name: None,
                property_implementation: None,
            });
        }
        targets.insert(implementation, name);
    }
    targets
}

/// The members of `owner` overriding an interface function, each with its own semantic
/// parameters: the entry is the member itself seen through the box, so its parameters are the
/// member's own, in their semantic types from before the carrier realization.
fn function_entries(
    ir: &IrFile,
    owner: TypeName,
) -> impl Iterator<Item = (u32, Vec<crate::ir::BridgeParameter>)> + '_ {
    let edges = ir
        .function_overrides
        .get(&owner)
        .map_or(&[][..], Vec::as_slice);
    edges
        .iter()
        .filter(move |edge| edge.implementation_owner == owner && edge.overridden_is_interface)
        .filter_map(move |edge| {
            let implementation = crate::jvm::bridges::implementation_function(ir, edge)?;
            let parameters = edge
                .implementation_parameter_identities
                .iter()
                .cloned()
                .zip(edge.implementation_parameters.iter().copied())
                .map(|(identity, semantic)| crate::ir::BridgeParameter { identity, semantic })
                .collect();
            Some((implementation, parameters))
        })
}

/// The accessors of `owner` overriding an interface accessor, each with its own semantic
/// parameters: the extension receiver of a member-extension property, then a setter's value. A
/// setter overrides only when the overridden property is itself mutable; a `var` implementing a
/// `val` adds a setter that overrides nothing and so has no entry.
fn accessor_entries(
    ir: &IrFile,
    owner: TypeName,
) -> impl Iterator<Item = (u32, Vec<crate::ir::BridgeParameter>)> + '_ {
    let edges = ir
        .property_overrides
        .get(&owner)
        .map_or(&[][..], Vec::as_slice);
    edges
        .iter()
        .filter(move |edge| edge.implementation_owner == owner && edge.overridden_is_interface)
        .flat_map(move |edge| {
            let (getter, setter) = implementation_accessors(ir, edge);
            let receiver =
                edge.implementation_receiver
                    .map(|semantic| crate::ir::BridgeParameter {
                        identity: crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
                        semantic,
                    });
            let getter = getter.map(|getter| (getter, receiver.iter().cloned().collect()));
            let setter = setter
                .filter(|_| edge.overridden_mutable && edge.implementation_mutable)
                .map(|setter| {
                    let value = crate::ir::BridgeParameter {
                        identity: crate::fir::ResolvedParameterIdentity::PropertySetterValue,
                        semantic: edge.implementation_type,
                    };
                    (setter, receiver.iter().cloned().chain([value]).collect())
                });
            getter.into_iter().chain(setter)
        })
}

/// The common-IR accessors realizing the implementation of `edge`: those the edge names for a
/// compiler-generated implementation, else those laid out for the selected source property.
fn implementation_accessors(
    ir: &IrFile,
    edge: &crate::ir::IrPropertyOverride,
) -> (Option<u32>, Option<u32>) {
    if edge.implementation_getter.is_some() || edge.implementation_setter.is_some() {
        return (edge.implementation_getter, edge.implementation_setter);
    }
    let crate::fir::ResolvedPropertyOverrideTarget::Module(property) = edge.implementation else {
        return (None, None);
    };
    match ir.local_property_layouts.get(&property) {
        Some(crate::ir::IrLocalPropertyLayout::Member { getter, setter, .. }) => (*getter, *setter),
        Some(crate::ir::IrLocalPropertyLayout::MemberExtension { getter, setter, .. }) => {
            (Some(*getter), *setter)
        }
        _ => (None, None),
    }
}
