//! JVM realization of backend-neutral local-class naming provenance.

use std::collections::HashMap;

use crate::ir::{ClassId, IrFile, IrLocalClassNameProvenance, IrLocalClassOwner, IrModuleSource};
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
    let name = provenance_name(provenance, facade, ir, cache);
    cache.insert(class, name);
    name
}

/// The JVM name a naming provenance describes: its lexical owner's physical name (or its source's
/// facade), then its source segments and generated ordinal.
fn provenance_name(
    provenance: &IrLocalClassNameProvenance,
    facade: &impl Fn(IrModuleSource) -> TypeName,
    ir: &IrFile,
    cache: &mut HashMap<ClassId, TypeName>,
) -> TypeName {
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
    name
}

/// Rename every local classifier to its JVM name and name every source callable reference's
/// class. `facade` names the file facade of a source, which roots a local classifier or reference
/// declared outside any classifier.
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
    let references = ir
        .callable_reference_provenance
        .iter()
        .map(|(&expression, provenance)| {
            (
                expression,
                provenance_name(provenance, &facade, ir, &mut physical),
            )
        })
        .collect();
    ir.callable_reference_names = references;
    let identities = physical
        .into_iter()
        .map(|(class, physical)| (ir.classes[class as usize].fq_name, physical))
        .collect();
    ir.remap_classifier_identities(&identities);
}

/// The class a source callable reference at `expression` compiles to, as [`realize`] named it.
/// `None` for a reference the naming walk never saw, one lowering synthesized.
pub(crate) fn callable_reference_name(ir: &IrFile, expression: u32) -> Option<TypeName> {
    ir.callable_reference_names.get(&expression).copied()
}

/// The class the source naming walk gave the lambda implemented by `impl_fn`. A pass that rebuilt
/// the enclosing body may have moved the lambda to a fresh node; the name stays with the source
/// literal's own node, and both build the same implementation.
pub(crate) fn lambda_class_name(ir: &IrFile, impl_fn: u32) -> Option<TypeName> {
    let mut names = ir
        .callable_reference_names
        .iter()
        .filter(|(&node, _)| {
            matches!(ir.exprs.get(node as usize), Some(crate::ir::IrExpr::Lambda { impl_fn: f, .. }) if *f == impl_fn)
        })
        .map(|(_, &name)| name);
    let name = names.next()?;
    names.all(|other| other == name).then_some(name)
}
