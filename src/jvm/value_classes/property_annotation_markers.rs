//! Value-class JVM names of synthetic property-annotation carriers.
//!
//! The marker is selected by stable property identity and follows that property's exact final
//! getter name. It does not participate in ordinary callable mangling: the main value-class pass
//! erases its parameters, while this collaborator owns only the property-to-getter name edge.

use super::member_names::vc_mangle;
use super::Under;
use crate::ir::IrFile;

pub(super) fn rename(ir: &mut IrFile, under: &Under) {
    let markers = ir
        .property_annotation_markers
        .iter()
        .map(|(&(owner, source_order), &marker)| (owner, source_order, marker))
        .collect::<Vec<_>>();
    let mut names = Vec::with_capacity(markers.len());
    for (owner, source_order, marker) in markers {
        let class = ir
            .classes
            .iter()
            .find(|class| class.fq_name_id() == owner)
            .expect("a property annotation marker retains its declaring class");
        let getter = if let Some(property) = class
            .properties
            .iter()
            .find(|property| property.source_order == source_order)
        {
            property
                .getter
                .map(|getter| ir.functions[getter as usize].name.clone())
                .or_else(|| property.getter_jvm_name.clone())
                .unwrap_or_else(|| crate::names::property_getter_name(&property.name))
        } else if let Some(property) = ir.member_ext_props.get(&owner).and_then(|properties| {
            properties
                .iter()
                .find(|property| property.source_order == source_order)
        }) {
            ir.functions[property.getter as usize].name.clone()
        } else if let Some(property) = ir
            .companion_blocks
            .properties_of(owner)
            .find(|property| property.source_order == source_order)
        {
            property.getter.map_or_else(
                || {
                    vc_mangle(
                        &crate::names::property_getter_name(&property.name),
                        &[],
                        &property.ty,
                        under,
                        false,
                        false,
                    )
                },
                |getter| ir.functions[getter as usize].name.clone(),
            )
        } else {
            let property = ir
                .declared_class_statics
                .get(&owner)
                .into_iter()
                .flatten()
                .map(|&static_id| &ir.statics[static_id as usize])
                .find(|property| property.source_order == source_order)
                .expect("a property annotation marker names its exact declaration");
            crate::names::property_getter_name(&property.name)
        };
        names.push((marker, format!("{getter}$annotations")));
    }
    for property in &ir.package_properties {
        let Some(marker) = ir.jvm_package_property_annotation_marker(property.property) else {
            continue;
        };
        let getter =
            crate::jvm::property_annotations::realized_package_getter_name(ir, property.property)
                .unwrap_or_else(|| crate::names::property_getter_name(&property.name));
        names.push((marker, format!("{getter}$annotations")));
    }
    for (marker, name) in names {
        ir.functions[marker as usize].name = name;
    }
}
