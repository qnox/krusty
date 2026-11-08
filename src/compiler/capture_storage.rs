//! Retire unread physical capture storage after complete source lowering.
//! Constructor-prefix parameters and their logical identities never change.

use crate::fir::ClassCaptureIdentity;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{
    ClassId, IrCtorArg, IrCtorParameterProvenance, IrExpr, IrFile, IrLocalPropertyLayout,
};

#[cfg(test)]
mod tests;

type Removed = HashMap<ClassId, HashSet<u32>>;
struct Plan {
    removed: Removed,
    remaps: HashMap<ClassId, Vec<Option<u32>>>,
}

/// Source-local ownership stays exact even when dependency/inline payloads share this IR file.
pub(super) fn source_owners(
    ir: &IrFile,
    index: &crate::fir::ResolvedModuleIndex,
    source: crate::fir::SourceFileId,
) -> HashSet<ClassId> {
    ir.checked_classifier_classes
        .iter()
        .filter_map(|(&declaration, &class)| {
            let header = index.declaration_header(declaration)?;
            if header.kind != crate::fir::DeclarationKind::Classifier
                || !header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            {
                return None;
            }
            let mut owner = Some(declaration);
            let mut seen = HashSet::default();
            while let Some(declaration) = owner {
                if !seen.insert(declaration) {
                    return None;
                }
                let anchor = index.declaration_anchor(declaration)?;
                let header = index.declaration_header(declaration)?;
                if anchor.source != source || header.flags.has(crate::fir::DeclarationFlags::INLINE)
                {
                    return None;
                }
                owner = anchor.owner;
            }
            Some(class)
        })
        .collect()
}

/// Only the completed-source handoff calls this after plugin source/runtime publication.
pub(super) fn retire(
    ir: &mut IrFile,
    plugins: &crate::plugins::registry::NativePlugins,
    source_owners: &HashSet<ClassId>,
) {
    // Enabled extensions still synthesize consumers in complete_enabled at target lowering.
    // Opaque operations cannot certify a closed field-use graph either.
    if !plugins.is_empty()
        || ir.compose.is_some()
        || ir
            .exprs
            .iter()
            .any(|e| matches!(e, IrExpr::PluginPlaceholder { .. }))
    {
        return;
    }
    let removed = candidates(ir, source_owners);
    if removed.is_empty() {
        return;
    }
    if let Some(plan) = prove_remap(ir, removed) {
        apply(ir, plan);
    }
}

fn candidates(ir: &IrFile, source_owners: &HashSet<ClassId>) -> Removed {
    let mut used = HashMap::<ClassId, HashSet<u32>>::default();
    let mut opaque = HashSet::default();
    // Include unattached templates/defaults as well as every nested/cross-class function. A
    // target's emitted-root inventory intentionally omits some of these deferred consumers.
    for expression in &ir.exprs {
        match expression {
            IrExpr::GetField { class, index, .. }
            | IrExpr::SetField { class, index, .. }
            | IrExpr::LateinitInitialized { class, index, .. } => {
                used.entry(*class).or_default().insert(*index);
            }
            IrExpr::EnclosingInstance { inner: owner, .. }
            | IrExpr::PropertyRead { owner, .. }
            | IrExpr::PropertyWrite { owner, .. } => {
                opaque.insert(*owner);
            }
            _ => {}
        }
    }
    let mut removed = Removed::default();
    for (id, class) in ir.classes.iter().enumerate() {
        let id = id as ClassId;
        if !source_owners.contains(&id) || !eligible_class(ir, id, &opaque) {
            continue;
        }
        for (parameter, argument) in class
            .ctor_args
            .iter()
            .enumerate()
            .take(class.constructor_prefix_count as usize)
        {
            let parameter = parameter as u32;
            let Some(ClassCaptureIdentity::EnclosingClass(owner)) = argument.capture_identity
            else {
                continue;
            };
            let Some(field) = argument
                .field_index
                .filter(|&field| field < class.ctor_param_count)
            else {
                continue;
            };
            let Some(storage) = class.fields.get(field as usize) else {
                continue;
            };
            if argument.provenance != IrCtorParameterProvenance::Capture || !argument.is_field
                || ir.class_capture_identities.get(&(id, parameter)) != Some(&ClassCaptureIdentity::EnclosingClass(owner))
                || !storage.is_private() || !storage.is_final() || storage.ty != argument.ty
                || !enclosing_owner(ir, argument, owner)
                || used.get(&id).is_some_and(|fields| fields.contains(&field))
                || ir.shared_class_capture_fields.contains_key(&(id, field))
                || class.ctor_args.iter().filter(|other| other.field_index == Some(field)).count() != 1
                || class.pre_super_param_fields.iter().filter(|(p, f)| *p == parameter || *f == field).copied().collect::<Vec<_>>() != [(parameter, field)]
                || class.properties.iter().any(|p| p.backing_field == Some(field) || p.delegate_field == Some(field))
                || ir.member_ext_props.get(&class.fq_name).is_some_and(|properties| properties.iter().any(|p| p.delegate_field == Some(field)))
                || ir.local_property_layouts.values().any(|layout| matches!(layout, IrLocalPropertyLayout::Member { class, backing_field: Some(backing), .. } if *class == id && *backing == field))
                || ir.checked_interface_delegation_fields.iter().any(|((declaration, _), f)| ir.checked_classifier_classes.get(declaration) == Some(&id) && *f == field)
                || class.ctor_args.iter().any(|other| matches!(other.provenance, IrCtorParameterProvenance::InterfaceDelegate { field: f, .. } if f == field))
            { continue; }
            removed.entry(id).or_default().insert(field);
        }
    }
    removed
}

fn eligible_class(ir: &IrFile, id: ClassId, opaque: &HashSet<crate::types::TypeName>) -> bool {
    let class = &ir.classes[id as usize];
    class.is_source_declared && class.is_local_class && !class.is_inner_class
        && !class.is_compiler_generated && !class.is_script && !class.is_data
        && !class.is_value && !class.is_full_value && !class.is_enum && !class.is_enum_entry
        && !class.is_annotation && class.annotation_impl_of.is_none() && class.lambda.is_none()
        && class.prop_ref.is_none() && class.func_ref.is_none() && class.sam_wrapper.is_none()
        && !ir.has_field_signatures(class.fq_name)
        && class.field_annotations.is_empty() && !opaque.contains(&class.fq_name)
        && ir.checked_classifier_classes.values().any(|&owner| owner == id)
        // Legacy implicit parameter->field layouts have no authenticated physical coordinate.
        && class.ctor_args.iter().all(|argument| !argument.is_field || argument.field_index.is_some())
        && !ir.specialized_anonymous_classes.iter().any(|(&copy, source)| copy == id || source.source == id)
}

fn enclosing_owner(ir: &IrFile, argument: &IrCtorArg, expected: crate::types::TypeName) -> bool {
    let Some(crate::ir::IrCapturedReceiver::Enclosing {
        classifier,
        owner: Some(owner),
    }) = argument
        .capture
        .as_ref()
        .and_then(|capture| capture.receiver.as_ref())
    else {
        return false;
    };
    owner.kind == crate::fir::EnclosingInstanceKind::Classifier
        && *classifier == expected
        && ir
            .checked_classifier_classes
            .get(&owner.declaration)
            .and_then(|&id| ir.classes.get(id as usize))
            .is_some_and(|declaration| declaration.fq_name_id() == expected)
}

fn prove_remap(ir: &mut IrFile, mut removed: Removed) -> Option<Plan> {
    let mut remaps = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(class, _)| removed.contains_key(&(*class as ClassId)))
        .map(|(class, declaration)| {
            let mut next = 0;
            let fields = declaration
                .fields
                .iter()
                .enumerate()
                .map(|(field, _)| {
                    if removed
                        .get(&(class as ClassId))
                        .is_some_and(|fields| fields.contains(&(field as u32)))
                    {
                        None
                    } else {
                        let field = next;
                        next += 1;
                        Some(field)
                    }
                })
                .collect::<Vec<_>>();
            (class as ClassId, fields)
        })
        .collect::<HashMap<_, _>>();
    let remap = |class, field: u32| {
        remaps
            .get(&class)
            .and_then(|fields| fields.get(field as usize))
            .copied()
            .flatten()
    };
    let mut incomplete = HashSet::default();
    physical_indices(ir, |class, field, droppable| {
        if remaps.contains_key(&class)
            && remap(class, *field).is_none()
            && !(droppable
                && removed
                    .get(&class)
                    .is_some_and(|fields| fields.contains(field)))
        {
            incomplete.insert(class);
        }
    })?;
    for &(class, field) in ir.shared_class_capture_fields.keys() {
        if remaps.contains_key(&class) && remap(class, field).is_none() {
            incomplete.insert(class);
        }
    }
    // Orphan/template coordinates are not required to be valid active-body coordinates. An
    // incomplete owner proof preserves that owner's storage and its original downstream behavior.
    for class in incomplete {
        removed.remove(&class);
        remaps.remove(&class);
    }
    (!removed.is_empty()).then_some(Plan { removed, remaps })
}

fn apply(ir: &mut IrFile, plan: Plan) {
    let Plan { removed, remaps } = plan;
    let remap = |class, field: u32| {
        remaps
            .get(&class)
            .and_then(|fields| fields.get(field as usize))
            .copied()
            .flatten()
    };
    // The complete preflight above is read-only. Only now retire the metadata stores, keeping
    // every source expression and constructor argument (including its evaluation) intact.
    for (&id, fields) in &removed {
        let class = &mut ir.classes[id as usize];
        class.ctor_param_count -= fields.len() as u32;
        class
            .pre_super_param_fields
            .retain(|(_, field)| !fields.contains(field));
        for argument in &mut class.ctor_args {
            if argument
                .field_index
                .is_some_and(|field| fields.contains(&field))
            {
                argument.is_field = false;
                argument.field_index = None;
            }
        }
        let mut field = 0;
        class.fields.retain(|_| {
            let keep = !fields.contains(&field);
            field += 1;
            keep
        });
    }
    physical_indices(ir, |class, field, _| {
        if remaps.contains_key(&class) {
            *field = remap(class, *field).expect("preflighted field");
        }
    })
    .expect("preflighted physical field owners");
    ir.shared_class_capture_fields = std::mem::take(&mut ir.shared_class_capture_fields)
        .into_iter()
        .map(|((class, field), ty)| {
            (
                (
                    class,
                    if remaps.contains_key(&class) {
                        remap(class, field).expect("preflighted shared field")
                    } else {
                        field
                    },
                ),
                ty,
            )
        })
        .collect();
}

/// The physical-coordinate surfaces. Logical capture identities/order are deliberately absent.
fn physical_indices(ir: &mut IrFile, mut visit: impl FnMut(ClassId, &mut u32, bool)) -> Option<()> {
    for expression in &mut ir.exprs {
        match expression {
            IrExpr::GetField { class, index, .. }
            | IrExpr::SetField { class, index, .. }
            | IrExpr::LateinitInitialized { class, index, .. } => visit(*class, index, false),
            _ => {}
        }
    }
    for (id, class) in ir.classes.iter_mut().enumerate() {
        let id = id as ClassId;
        for argument in &mut class.ctor_args {
            if let Some(field) = &mut argument.field_index {
                visit(
                    id,
                    field,
                    argument.provenance == IrCtorParameterProvenance::Capture,
                );
            }
            if let IrCtorParameterProvenance::InterfaceDelegate { field, .. } =
                &mut argument.provenance
            {
                visit(id, field, false);
            }
        }
        for (_, field) in &mut class.pre_super_param_fields {
            visit(id, field, true);
        }
        for property in &mut class.properties {
            for field in property
                .backing_field
                .iter_mut()
                .chain(property.delegate_field.iter_mut())
            {
                visit(id, field, false);
            }
        }
        if let Some(properties) = ir.member_ext_props.get_mut(&class.fq_name) {
            for property in properties {
                if let Some(field) = &mut property.delegate_field {
                    visit(id, field, false);
                }
            }
        }
    }
    for layout in ir.local_property_layouts.values_mut() {
        if let IrLocalPropertyLayout::Member {
            class,
            backing_field: Some(field),
            ..
        } = layout
        {
            visit(*class, field, false);
        }
    }
    for ((owner, _), field) in &mut ir.checked_interface_delegation_fields {
        let class = ir.checked_classifier_classes.get(owner).copied()?;
        visit(class, field, false);
    }
    Some(())
}
