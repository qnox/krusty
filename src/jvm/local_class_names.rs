//! JVM realization of backend-neutral local-class naming provenance.

use std::collections::HashMap;

mod continuations;

pub(crate) use continuations::{continuation_class_name, continuation_ordinal, name_continuation};

use crate::ir::{
    ClassId, EnclosingDeclaration, IrFile, IrLocalClassNameProvenance, IrLocalClassOwner,
    IrModuleSource,
};
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

/// kotlinc's `fqNameWhenAvailable` of the class a provenance describes: its lexical owner's, then
/// the declarations enclosing it, then its own name (`<no name provided>` for an unnamed one).
/// A companion's static initialization runs in its outer class's `<clinit>`, so kotlinc nests a
/// class declared there under the outer class.
fn declaration_path(
    provenance: &IrLocalClassNameProvenance,
    facade: &impl Fn(IrModuleSource) -> TypeName,
    ir: &IrFile,
) -> String {
    let mut path = match provenance.lexical_owner {
        Some(IrLocalClassOwner::Class(owner)) => match ir.local_class_name_provenance.get(&owner) {
            Some(owner) => declaration_path(owner, facade, ir),
            None => {
                let class = &ir.classes[owner as usize];
                let static_initializer =
                    provenance.parents.first() == Some(&EnclosingDeclaration::StaticInitializer);
                match class.fq_name.nested_owner() {
                    Some(outer)
                        if class.is_companion
                            && static_initializer
                            && !outer_is_interface(ir, outer) =>
                    {
                        qualified_name(outer)
                    }
                    _ => qualified_name(class.fq_name),
                }
            }
        },
        Some(IrLocalClassOwner::External(owner)) => qualified_name(owner),
        None => qualified_name(facade(provenance.source)),
    };
    for parent in provenance.parents.iter() {
        path.push('.');
        push_declaration_name(&mut path, parent);
    }
    path.push('.');
    match provenance.ordinal {
        Some(_) => path.push_str("<no name provided>"),
        None => path.push_str(
            provenance
                .segments
                .last()
                .expect("a named local class carries its source name"),
        ),
    }
    path
}

/// The name kotlinc's IR gives an enclosing declaration: the JVM method a role lowers into
/// (`<init>`, `<clinit>`, `<get-x>`), `<anonymous>` for a lambda, or the source name.
fn push_declaration_name(path: &mut String, declaration: &EnclosingDeclaration) {
    match declaration {
        EnclosingDeclaration::Function(name) | EnclosingDeclaration::EnumEntry(name) => {
            path.push_str(name)
        }
        EnclosingDeclaration::Getter(property) => {
            path.push_str("<get-");
            path.push_str(property);
            path.push('>');
        }
        EnclosingDeclaration::Setter(property) => {
            path.push_str("<set-");
            path.push_str(property);
            path.push('>');
        }
        EnclosingDeclaration::Lambda => path.push_str("<anonymous>"),
        EnclosingDeclaration::InstanceInitializer => path.push_str("<init>"),
        EnclosingDeclaration::StaticInitializer => path.push_str("<clinit>"),
    }
}

/// Whether `outer` is an interface or annotation class of this file. Its companion keeps its static
/// state, so the companion's initializers run in its own `<clinit>`.
fn outer_is_interface(ir: &IrFile, outer: TypeName) -> bool {
    ir.classes
        .iter()
        .any(|class| class.fq_name == outer && (class.is_interface || class.is_annotation))
}

/// kotlinc's `fqNameWhenAvailable` of the class a suspend function's continuation class is
/// declared in: `host`'s, or the file facade's when the function has no dispatch receiver.
fn host_path(ir: &IrFile, host: Option<TypeName>, facade: &str) -> String {
    match host {
        Some(host) => ir
            .declaration_paths
            .get(&host)
            .cloned()
            .unwrap_or_else(|| qualified_name(host)),
        None => facade.replace('/', "."),
    }
}

/// The dotted qualified name of a classifier declared outside executable code, as kotlinc's
/// `FqName` spells it.
fn qualified_name(name: TypeName) -> String {
    if let Some(owner) = name.nested_owner() {
        if let Some(nested) = name.nested_segment_within(owner) {
            return format!("{}.{nested}", qualified_name(owner));
        }
    }
    match name.parent().filter(|parent| *parent != TypeName::ROOT) {
        Some(parent) => format!("{}.{}", qualified_name(parent), name.segment_ref()),
        None => name.segment(),
    }
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
    let mut paths = HashMap::new();
    for class in classes {
        let name = physical_name(class, &facade, ir, &mut physical);
        paths.insert(
            name,
            declaration_path(&ir.local_class_name_provenance[&class], &facade, ir),
        );
    }
    let references = ir
        .callable_reference_provenance
        .iter()
        .map(|(&expression, provenance)| {
            let name = provenance_name(provenance, &facade, ir, &mut physical);
            paths.insert(name, declaration_path(provenance, &facade, ir));
            (expression, name)
        })
        .collect();
    ir.callable_reference_names = references;
    ir.declaration_paths = paths;
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
