//! Inventory of exact value-class declarations referenced by one JVM IR file.
//!
//! This boundary combines normalized checked classifier facts with common-IR facts, validates that
//! duplicate publications agree, and follows declared underlying types transitively. The pass-local
//! JVM erasure map converts only semantic scalar spellings; boxing, storage, and descriptors remain
//! owned by their later representation operations.

use super::representation::has_native_carrier;
use super::Under;
use crate::ir::referenced_classifiers::{collect_classifier_names, referenced_classifier_names};
use crate::ir::IrFile;
use crate::types::TypeName;
use std::collections::{HashMap, HashSet};

pub(super) fn merge_referenced(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    declarations: &mut Under,
) -> Option<HashMap<TypeName, String>> {
    let mut underlying_properties = HashMap::new();
    let mut pending = referenced_classifier_names(ir);
    let mut probed = HashSet::new();
    while let Some(classifier) = pending.pop() {
        if !probed.insert(classifier) {
            continue;
        }
        if let Some(underlying) = declarations.get(&classifier).copied() {
            collect_classifier_names(underlying, &mut pending);
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
        collect_classifier_names(underlying, &mut pending);
        // A value class the type model carries as a native scalar (the unsigned integers) is still
        // a checked value-class declaration, but its JVM slot is that scalar: it stays out of the
        // rewrite map, whose entries box and unbox through the class's own `box-impl`.
        if has_native_carrier(classifier) {
            continue;
        }
        declarations.insert(
            classifier,
            underlying.scalar_value_repr().unwrap_or(underlying),
        );
    }
    crate::value_classes::declarations_are_acyclic(declarations).then_some(underlying_properties)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    struct UnsignedFacts;

    impl crate::types::ClassifierFactSource for UnsignedFacts {
        fn classifier_annotations(
            &self,
            _classifier: TypeName,
        ) -> Option<Vec<crate::types::ResolvedAnnotation>> {
            None
        }

        fn classifier_value_underlying(&self, classifier: TypeName) -> Option<Ty> {
            classifier.matches("kotlin/UInt").then_some(Ty::Int)
        }
    }

    #[test]
    fn checked_unsigned_declaration_is_semantic_but_not_rewritten() {
        let uint = crate::types::type_name("kotlin/UInt");
        let mut ir = IrFile::default();
        let mut holder = crate::plugins::synthetic_class("fixture/Holder");
        holder.fields.push(crate::ir::IrField::new(
            "value".to_string(),
            Ty::obj_name(uint),
        ));
        ir.add_class(holder);
        let mut declarations = Under::new();

        assert!(merge_referenced(&mut ir, &UnsignedFacts, &mut declarations).is_some());
        assert_eq!(ir.value_class_underlying_name(uint), Some(Ty::Int));
        assert!(ir.is_value_class_name(uint));
        assert!(!super::super::is_boxed_value_class(&ir, uint));
        assert!(!declarations.contains_key(&uint));
    }

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
