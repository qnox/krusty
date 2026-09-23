//! Inventory of exact value-class declarations referenced by one JVM IR file.
//!
//! Which classifiers a file references is the shared IR walk ([`IrFile::referenced_classifiers`]);
//! what this file adds is the JVM's bookkeeping of the facts it finds.
//!
//! This boundary combines normalized checked classifier facts with common-IR facts, validates that
//! duplicate publications agree, and follows declared underlying types transitively. The pass-local
//! JVM erasure map converts only semantic scalar spellings; boxing, storage, and descriptors remain
//! owned by their later representation operations.

use super::{is_native_unsigned, Under};
use crate::ir::IrFile;
use crate::types::TypeName;
use std::collections::{HashMap, HashSet};

pub(super) fn merge_referenced(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    declarations: &mut Under,
) -> Option<HashMap<TypeName, String>> {
    let mut underlying_properties = HashMap::new();
    let mut pending = ir.referenced_classifiers();
    let mut probed = HashSet::new();
    while let Some(classifier) = pending.pop() {
        if !probed.insert(classifier) || is_native_unsigned(classifier) {
            continue;
        }
        if let Some(underlying) = declarations.get(&classifier).copied() {
            crate::ir::collect_classifiers(underlying, &mut pending);
            continue;
        }
        if crate::types::prim_array_element(classifier).is_some() {
            continue;
        }

        if let Some(property) = classifiers.classifier_value_property(classifier) {
            crate::trace_compiler!(
                "value_classes",
                "external value class {} underlying property {}",
                classifier,
                property
            );
            underlying_properties.insert(classifier, property);
        }

        let candidates = [
            ir.external_value_class_name(classifier).copied(),
            classifiers.classifier_value_underlying(classifier),
        ];
        let mut declared = None;
        for candidate in candidates.into_iter().flatten() {
            let candidate = candidate.canonical_semantic();
            if let Some(existing) = declared {
                assert_eq!(
                    existing, candidate,
                    "checked providers disagreed about one value-class declaration"
                );
            } else {
                declared = Some(candidate);
            }
        }
        let Some(underlying) = declared else {
            continue;
        };
        ir.insert_external_value_class_name(classifier, underlying);
        declarations.insert(
            classifier,
            underlying.scalar_value_repr().unwrap_or(underlying),
        );
        crate::ir::collect_classifiers(underlying, &mut pending);
    }
    crate::value_classes::declarations_are_acyclic(declarations).then_some(underlying_properties)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    #[test]
    fn cyclic_checked_facts_decline_instead_of_selecting_an_edge() {
        let first = crate::types::type_name("fixture/First");
        let second = crate::types::type_name("fixture/Second");
        let mut ir = IrFile::default();
        ir.insert_external_value_class_name(first, Ty::obj_name(second));
        ir.insert_external_value_class_name(second, Ty::obj_name(first));
        let mut holder = crate::plugins::synthetic_class("fixture/Holder");
        holder.fields.push(crate::ir::IrField::new(
            "value".to_string(),
            Ty::obj_name(first),
        ));
        ir.add_class(holder);
        let mut declarations = Under::new();

        assert_eq!(
            merge_referenced(
                &mut ir,
                &crate::libraries::EmptySymbolSource,
                &mut declarations,
            ),
            None
        );
    }
}
