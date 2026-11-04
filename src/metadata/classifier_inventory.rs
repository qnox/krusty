//! Declaration ownership and order of the nested classifiers listed in Class metadata.

use crate::ir::{IrClass, IrFile};
use crate::types::ClassifierDeclarationName;

/// Direct declared children in source order, then published generated names and the companion.
pub(crate) fn nested_classifiers(ir: &IrFile, class: &IrClass) -> Vec<String> {
    let mut declared = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.is_source_declared
                && !candidate.is_compiler_generated
                && candidate.enclosure.is_none()
        })
        .filter_map(|(index, candidate)| {
            let segment = if crate::metadata::local_classifiers::is_local(ir, candidate) {
                // Local implementation classes retain their checked naming provenance. JVM
                // realization may rename their identities before recording its metadata inventory.
                if candidate.fq_name.nested_owner() != Some(class.fq_name) {
                    return None;
                }
                candidate
                    .fq_name
                    .nested_segment_within(class.fq_name)
                    .expect("a recorded local child belongs to its exact owner")
                    .to_string()
            } else {
                match candidate.declaration_name.as_ref()? {
                    ClassifierDeclarationName::Nested { owner, name }
                        if *owner == class.fq_name =>
                    {
                        name.to_string()
                    }
                    ClassifierDeclarationName::TopLevel { .. }
                    | ClassifierDeclarationName::Nested { .. } => return None,
                }
            };
            Some((
                ir.class_source_order(index as crate::ir::ClassId)
                    .expect("a source classifier carries its stable declaration order"),
                segment,
            ))
        })
        .collect::<Vec<_>>();
    declared.sort_by_key(|(order, _)| *order);
    let mut names = declared
        .into_iter()
        .map(|(_, segment)| segment)
        .collect::<Vec<_>>();
    // A plugin publishes the exact name of each generated classifier Kotlin code can name.
    // These follow declared children; a companion the plugin adds comes last.
    names.extend(class.published_nested_classifiers.iter().cloned());
    if let Some(companion) = class.companion_class {
        let segment = companion
            .nested_segment_within(class.fq_name)
            .expect("a declared companion belongs to its exact owner");
        if !names.iter().any(|name| name == segment) {
            names.push(segment.to_string());
        }
    }
    names
}
