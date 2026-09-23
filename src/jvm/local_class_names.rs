//! JVM realization of backend-neutral local-class naming provenance.

use std::collections::HashMap;

use crate::ir::{ClassId, IrFile};
use crate::types::{type_name, type_name_nested_child, TypeName};

fn physical_name(
    class: ClassId,
    facade: TypeName,
    ir: &IrFile,
    cache: &mut HashMap<ClassId, TypeName>,
) -> TypeName {
    if let Some(name) = cache.get(&class) {
        return *name;
    }
    let provenance = ir
        .local_class_name_provenance
        .get(&class)
        .expect("a requested local classifier must carry naming provenance");
    let mut name = provenance
        .lexical_owner
        .map(|owner| physical_name(owner, facade, ir, cache))
        .unwrap_or(facade);
    for segment in provenance.segments.iter() {
        name = type_name_nested_child(name, segment);
    }
    if let Some(ordinal) = provenance.ordinal {
        name = type_name_nested_child(name, &ordinal.to_string());
    }
    cache.insert(class, name);
    name
}

pub(crate) fn realize(ir: &mut IrFile, facade: &str) {
    let facade = type_name(facade);
    let classes = ir
        .local_class_name_provenance
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let mut physical = HashMap::new();
    for class in classes {
        physical_name(class, facade, ir, &mut physical);
    }
    let identities = physical
        .into_iter()
        .map(|(class, physical)| (ir.classes[class as usize].fq_name, physical))
        .collect();
    ir.remap_classifier_identities(&identities);
}
