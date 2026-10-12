//! Removing instance fields a JVM storage realization retired.
//!
//! A class's fields are addressed by index everywhere: field operations, the property declarations
//! that own them, and their checked layouts. A pass that moves or drops storage (companion
//! hoisting, an in-place delegate) first rewrites every operation on the retired field, then
//! compacts the table here so the surviving fields keep exactly the identity they had.

use std::collections::{HashMap, HashSet};

use crate::ir::{ClassId, IrExpr, IrFile};

/// Drop each class's `removed` fields and retarget every surviving field index. Every operation on
/// a removed field must already have been rewritten; one that survives is a broken realization.
pub(crate) fn remove_fields(ir: &mut IrFile, removed: &HashMap<ClassId, HashSet<u32>>) {
    let mut remaps = HashMap::<ClassId, Vec<Option<u32>>>::new();
    for (&class, fields_removed) in removed {
        let fields = std::mem::take(&mut ir.classes[class as usize].fields);
        let mut retained = Vec::with_capacity(fields.len());
        let mut remap = Vec::with_capacity(fields.len());
        for (old, field) in fields.into_iter().enumerate() {
            if fields_removed.contains(&(old as u32)) {
                remap.push(None);
            } else {
                remap.push(Some(retained.len() as u32));
                retained.push(field);
            }
        }
        ir.classes[class as usize].fields = retained;
        remaps.insert(class, remap);
    }
    for expression in &mut ir.exprs {
        match expression {
            IrExpr::GetField { class, index, .. }
            | IrExpr::SetField { class, index, .. }
            | IrExpr::LateinitInitialized { class, index, .. } => {
                if let Some(remap) = remaps.get(class) {
                    *index = remap[*index as usize]
                        .expect("a retired field's operations are rewritten before compaction");
                }
            }
            _ => {}
        }
    }
    retarget_surviving_field_indices(ir, &remaps);
}

/// A property that keeps its storage still names its field after siblings are removed.
///
/// Expression field operations are retargeted with the table; the declaration's own field index is
/// a separate identity and moves with them. A `var` that only customizes its setter synthesizes its
/// default getter from that index, so the getter still addresses the field the setter writes.
fn retarget_surviving_field_indices(ir: &mut IrFile, remaps: &HashMap<ClassId, Vec<Option<u32>>>) {
    for (&class, remap) in remaps {
        for property in &mut ir.classes[class as usize].properties {
            property.backing_field = retarget_field_index(remap, property.backing_field);
            property.delegate_field = retarget_field_index(remap, property.delegate_field);
        }
        let owner = ir.classes[class as usize].fq_name;
        if let Some(properties) = ir.member_ext_props.get_mut(&owner) {
            for property in properties {
                property.delegate_field = retarget_field_index(remap, property.delegate_field);
            }
        }
    }
    for layout in ir.local_property_layouts.values_mut() {
        if let crate::ir::IrLocalPropertyLayout::Member {
            class,
            backing_field,
            ..
        } = layout
        {
            if let Some(remap) = remaps.get(class) {
                *backing_field = retarget_field_index(remap, *backing_field);
            }
        }
    }
}

fn retarget_field_index(remap: &[Option<u32>], index: Option<u32>) -> Option<u32> {
    index.and_then(|index| remap[index as usize])
}
