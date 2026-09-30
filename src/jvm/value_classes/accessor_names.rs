//! JVM names of synthesized value-class property accessors.
//!
//! A property the backend synthesizes (`getter` absent, backing field present) has no callable
//! of its own. Its JVM spelling is a fact of that property. A call is renamed only when an
//! earlier JVM pass bound that exact call to the property. Bridges of property-getter and
//! property-setter kind delegate to the same accessor; function bridges are other declarations
//! and keep their targets.

use super::member_names::vc_mangle;
use super::Under;
use crate::ir::{BridgeKind, Callee, IrExpr, IrFile};
use crate::jvm::names::property_getter_name;
use crate::names::property_setter_name;
use crate::types::Ty;

pub(super) fn stamp_synthesized(ir: &mut IrFile, under: &Under) {
    for class_index in 0..ir.classes.len() {
        let properties: Vec<(usize, String, Ty)> = ir.classes[class_index]
            .properties
            .iter()
            .enumerate()
            .filter(|(_, property)| property.getter.is_none() && property.backing_field.is_some())
            .map(|(index, property)| (index, property.name.clone(), property.ty))
            .collect();
        for (index, name, ty) in properties {
            let declared_value_class = ty
                .non_null()
                .obj_internal()
                .is_some_and(|fq_name| under.contains_key(&fq_name));
            // The property's own accessor is mangled only when its own type is a value class.
            // When it merely overrides a value-class property (`override val p: Nothing?`), the
            // own accessor keeps the plain spelling and its bridge carries the supertype's name.
            if !declared_value_class {
                continue;
            }
            let plain_getter = property_getter_name(&name);
            let getter = vc_mangle(&plain_getter, &[], &ty, under, false, false);
            let plain_setter = property_setter_name(&name);
            let setter = vc_mangle(
                &plain_setter,
                std::slice::from_ref(&ty),
                &Ty::Unit,
                under,
                false,
                false,
            );
            rename_bound_calls(ir, class_index as u32, index as u32, &getter);
            retarget_property_bridges(
                ir,
                class_index,
                &plain_getter,
                &getter,
                &plain_setter,
                &setter,
            );
            let property = &mut ir.classes[class_index].properties[index];
            property.getter_jvm_name = Some(getter);
            property.setter_jvm_name = Some(setter);
        }
    }
}

fn rename_bound_calls(ir: &mut IrFile, class: u32, property: u32, getter: &str) {
    let calls = ir
        .synthesized_accessor_calls
        .iter()
        .filter(|(_, binding)| binding.class == class && binding.property == property)
        .map(|(call, _)| *call)
        .collect::<Vec<_>>();
    for call in calls {
        let Some(expression) = ir.exprs.get_mut(call as usize) else {
            continue;
        };
        if let IrExpr::Call {
            callee: Callee::Virtual { name, .. },
            ..
        } = expression
        {
            *name = getter.to_string();
        }
    }
}

fn retarget_property_bridges(
    ir: &mut IrFile,
    class_index: usize,
    plain_getter: &str,
    getter: &str,
    plain_setter: &str,
    setter: &str,
) {
    for bridge in &mut ir.classes[class_index].bridges {
        let target = bridge
            .target_name
            .as_deref()
            .unwrap_or(bridge.name.as_str());
        match bridge.kind {
            BridgeKind::PropertyGetter if target == plain_getter => {
                bridge.target_name = Some(getter.to_string());
            }
            BridgeKind::PropertySetter if target == plain_setter => {
                bridge.target_name = Some(setter.to_string());
            }
            _ => {}
        }
    }
}
