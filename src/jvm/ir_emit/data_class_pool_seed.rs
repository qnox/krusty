//! The constant-pool entries a data class's SYNTHESIZED members need, reserved in kotlinc's order.
//!
//! `equals`/`hashCode`/`toString`/`componentN`/`copy` are emitted after the declared members, but
//! their pool entries are interned before them, so the names they reference are reserved here
//! rather than at their emission sites.

use super::*;

/// What the seeding reads from the class pool pass around it, mirroring `PlainClassPoolSeed`: the
/// descriptors and signatures that pass has already computed for the DECLARED members, which the
/// synthesized ones reuse rather than recompute.
pub(super) struct DataClassPoolSeed<'a> {
    pub ir: &'a IrFile,
    pub class: &'a crate::ir::IrClass,
    pub bodies: &'a dyn MethodBodies,
    pub fq_name: &'a str,
    pub ctor_signature: Option<&'a str>,
    pub ctor_desc: &'a str,
    pub field_sigs: &'a [Option<String>],
    pub field_sig_of: &'a dyn Fn(&crate::ir::IrField) -> Option<String>,
}

/// Reserve the pool entries for the class's synthesized data-class members.
pub(super) fn seed_data_class_members(seed: DataClassPoolSeed<'_>, cw: &mut ClassWriter) {
    let DataClassPoolSeed {
        ir,
        class: c,
        bodies,
        fq_name,
        ctor_signature,
        ctor_desc,
        field_sigs,
        field_sig_of,
    } = seed;
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    // The class's SIMPLE name, which a data class's `toString` renders: `Inner(id=…)`, not
    // `Holder$Inner(id=…)`. Read through the identity tree, which is where the nesting relation
    // lives — the rendered internal name spells a package separator and a nesting separator
    // differently, and splitting it on one of them left a nested class carrying its outer prefix
    // into the recipe. `fir_lower::data_classes` builds the twin recipe from the same operation.
    let simple = c.fq_name_id().nested_segment_ref();
    // The synthesized members cover the PRIMARY-CONSTRUCTOR properties only; a body property has a
    // backing field in `c.fields` but no `componentN` and no `copy` parameter (see
    // `build_class_metadata`, which takes the same prefix).
    let component_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
    let data_fields: Vec<(String, String)> = component_fields
        .iter()
        .map(|f| (f.name.clone(), desc(f.ty)))
        .collect();
    // Derive the JVM-only dispatch owner from each property's semantic type. The physical field
    // may already have been erased by a backend pass, so prefer the property declaration.
    let hashcode_owners: Vec<Option<String>> = component_fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let semantic_ty = c
                .properties
                .iter()
                .find(|property| property.backing_field == Some(index as u32))
                .map_or(field.ty, |property| property.ty);
            data_class_hashcode_owner(ir, bodies, semantic_ty)
        })
        .collect();
    let mut data_accessors = Vec::new();
    for property in &c.properties {
        if property.is_private {
            continue;
        }
        let Some(field) = property
            .backing_field
            .and_then(|index| c.fields.get(index as usize))
        else {
            continue;
        };
        let accessor_ty = declared_property_accessor_jvm(ir, property, field);
        let accessor_desc = desc(accessor_ty);
        let field_sig = field_sig_of(field);
        let getter = property
            .getter_jvm_name
            .clone()
            .unwrap_or_else(|| crate::names::property_getter_name(&property.name));
        data_accessors.push(crate::jvm::classfile::DataAccessorInfo {
            name: getter,
            desc: format!("(){accessor_desc}"),
            setter_kind: 0,
            signature: field_sig.as_ref().map(|signature| format!("(){signature}")),
        });
        if property.is_var {
            let setter = property
                .setter_jvm_name
                .clone()
                .unwrap_or_else(|| crate::names::property_setter_name(&property.name));
            let guarded = accessor_ty.is_reference()
                && !property.ty.is_nullable()
                && is_nonnull_reference_field(ir, fq_name, &field.name, field.ty);
            data_accessors.push(crate::jvm::classfile::DataAccessorInfo {
                name: setter,
                desc: format!("({accessor_desc})V"),
                setter_kind: if guarded { 2 } else { 1 },
                signature: field_sig.map(|signature| format!("({signature})V")),
            });
        }
    }
    // `copy`'s generic Signature shares the ctor's parameter list, returning `self` instead of `void`.
    let copy_sig = ctor_signature
        .and_then(|s| s.strip_suffix('V'))
        .map(|params| format!("{params}L{fq_name};"));
    cw.seed_data_class_pool(
        fq_name,
        ctor_desc,
        simple,
        &data_fields,
        &crate::jvm::classfile::DataMemberInfo {
            accessors: &data_accessors,
            hashcode_owners: &hashcode_owners,
            copy_sig: copy_sig.as_deref(),
            copy_is_private: data_copy_fid(ir, c)
                .is_some_and(|fid| ir.private_methods.contains(&fid)),
            field_sigs,
        },
    );
}
