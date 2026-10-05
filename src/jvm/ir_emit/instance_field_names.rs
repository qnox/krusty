//! Physical JVM names of instance backing fields.
//!
//! Kotlin permits an instance property and a companion property with the same source name, but the
//! JVM field signature does not include the STATIC flag. kotlinc therefore keeps the companion
//! static's source name and suffixes the instance backing field (`result` -> `result$1`). This is
//! solely a JVM realization decision: IR properties, metadata, accessors, and resolver identities
//! retain the source name.
//!
//! Names are assigned in declaration order. Each field sees the physical names already given to
//! earlier fields, computed once. Two delegated properties of one name (`val prop` and
//! `val String.prop`) both ask for `prop$delegate`; the later one becomes `prop$delegate$1`.

use crate::ir::{IrClass, IrField, IrFile};
use crate::jvm::names::type_descriptor;

use super::declaration_types::jvm_declared_ty;

pub(super) fn instance_field_jvm_name(ir: &IrFile, class: &IrClass, field: &IrField) -> String {
    let field_index = class
        .fields
        .iter()
        .position(|candidate| std::ptr::eq(candidate, field))
        .expect("an instance field name must belong to its class");
    let mut assigned = Vec::with_capacity(field_index + 1);
    for index in 0..=field_index {
        assigned.push(physical_name(ir, class, index, &assigned));
    }
    assigned
        .pop()
        .expect("the requested instance field was named")
}

fn physical_name(ir: &IrFile, class: &IrClass, index: usize, earlier: &[String]) -> String {
    if let Some(capture) = super::super::capture_names::field_capture(class, index) {
        return super::super::capture_names::class_capture(ir, class, capture).field;
    }
    if let Some(lambda) = &class.lambda {
        return super::super::capture_names::lambda_class_capture(ir, lambda, index)
            .expect("every field of a lambda class stores one of its captures")
            .field;
    }
    let field = &class.fields[index];
    let owner = class.fq_name();
    let descriptor = type_descriptor(jvm_declared_ty(&field.ty));
    let base = field.name.as_str();
    let earlier_has = |candidate: &str| earlier.iter().any(|name| name == candidate);
    let conflicts_with_static = |candidate: &str| {
        ir.statics
            .iter()
            .enumerate()
            .any(|(static_index, static_field)| {
                static_field.owner_matches(&owner)
                    && ir.static_field_jvm_name(static_index as u32) == candidate
                    && type_descriptor(jvm_declared_ty(&static_field.ty)) == descriptor
            })
    };
    if !conflicts_with_static(base) && !earlier_has(base) {
        return field.name.clone();
    }
    for suffix in 1usize.. {
        let candidate = format!("{base}${suffix}");
        if !earlier_has(&candidate)
            && !conflicts_with_static(&candidate)
            && !class.fields.iter().any(|other| {
                other.name == candidate && type_descriptor(jvm_declared_ty(&other.ty)) == descriptor
            })
        {
            return candidate;
        }
    }
    unreachable!("an unused JVM backing-field suffix always exists")
}
