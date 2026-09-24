//! JVM realization of backend-neutral local-class naming provenance.

use std::collections::HashMap;

use crate::ir::{ClassId, IrFile, IrLocalClassOwner, IrModuleSource};
use crate::types::{type_name_nested_child, TypeName};

fn physical_name(
    class: ClassId,
    facade: &impl Fn(IrModuleSource) -> TypeName,
    ir: &IrFile,
    cache: &mut HashMap<ClassId, TypeName>,
) -> TypeName {
    if let Some(name) = cache.get(&class) {
        return *name;
    }
    // An owner declared outside executable code (a top-level or member classifier) already has its
    // physical identity; only local and anonymous classifiers are named from provenance.
    let Some(provenance) = ir.local_class_name_provenance.get(&class) else {
        return ir.classes[class as usize].fq_name;
    };
    let mut name = match provenance.lexical_owner {
        Some(IrLocalClassOwner::Class(owner)) => physical_name(owner, facade, ir, cache),
        Some(IrLocalClassOwner::External(owner)) => owner,
        None => facade(provenance.source),
    };
    for segment in provenance.segments.iter() {
        name = type_name_nested_child(name, segment);
    }
    if let Some(ordinal) = provenance.ordinal {
        name = type_name_nested_child(name, &ordinal.to_string());
    }
    cache.insert(class, name);
    name
}

/// Rename every local classifier to its JVM name. `facade` names the file facade of a source,
/// which roots a local classifier declared outside any classifier.
pub(crate) fn realize(ir: &mut IrFile, facade: impl Fn(IrModuleSource) -> TypeName) {
    let classes = ir
        .local_class_name_provenance
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let mut physical = HashMap::new();
    for class in classes {
        physical_name(class, &facade, ir, &mut physical);
    }
    let identities = physical
        .into_iter()
        .map(|(class, physical)| (ir.classes[class as usize].fq_name, physical))
        .collect();
    ir.remap_classifier_identities(&identities);
}

/// The class a source callable reference at `expression` compiles to: its lexical owner's physical
/// name (after [`realize`]), then its source segments and ordinal. `None` for a reference the naming
/// walk never saw, one lowering synthesized.
pub(crate) fn callable_reference_name(
    ir: &IrFile,
    facade: &str,
    expression: u32,
) -> Option<TypeName> {
    let provenance = ir.callable_reference_provenance.get(&expression)?;
    let mut name = provenance
        .lexical_owner
        .map(|owner| ir.classes[owner as usize].fq_name)
        .unwrap_or_else(|| type_name(facade));
    for segment in provenance.segments.iter() {
        name = type_name_nested_child(name, segment);
    }
    if let Some(ordinal) = provenance.ordinal {
        name = type_name_nested_child(name, &ordinal.to_string());
    }
    Some(name)
}
