//! The member function slots of a source classifier, for each name whose declarations fall into
//! more than one slot.
//!
//! A classifier's function members are its own declarations and, for every inherited declaration
//! none of them overrides, one fake override that stands for each inherited declaration it merges.
//! Pass 1 groups the declarations of the complete applied hierarchy by those override relations
//! while the declaration providers are live. A target whose realization can make two distinct
//! slots one physical member (for example, two signatures with the same JVM erasure) checks the
//! records without comparing declarations again.

use super::{DeclarationId, ResolvedFunctionOverrideTarget, ResolvedModuleIndex};
use super::{ResolvedParameterIdentity, ResolvedTy};
use crate::types::TypeName;

/// One declaration in a slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSlotFunction {
    pub target: ResolvedFunctionOverrideTarget,
    pub owner: TypeName,
    /// Whether the classifier itself declares it.
    pub declared_by_classifier: bool,
    /// The declaration's own parameters (context parameters, then an extension receiver, then the
    /// value parameters) and result, before any substitution.
    pub declared_parameters: Box<[ResolvedTy]>,
    pub declared_result: ResolvedTy,
    /// The same parameters and result as the classifier sees them through its applied supertype.
    pub applied_parameters: Box<[ResolvedTy]>,
    pub applied_result: ResolvedTy,
    pub parameter_identities: Box<[ResolvedParameterIdentity]>,
    pub suspend: bool,
}

/// The slots of one function name. Each slot lists the classifier's own declaration first, when it
/// has one, and then the inherited declarations the slot stands for, nearest supertype first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFunctionSlotFamily {
    pub name: Box<str>,
    pub slots: Box<[Box<[ResolvedSlotFunction]>]>,
}

impl ResolvedModuleIndex {
    /// The function names of `classifier` that hold more than one slot. Absence means the
    /// classifier's override plan has not been finalized.
    pub fn function_slot_families(
        &self,
        classifier: DeclarationId,
    ) -> Option<&[ResolvedFunctionSlotFamily]> {
        self.function_slot_families
            .get(&classifier)
            .map(Box::as_ref)
    }

    pub(crate) fn publish_function_slot_families(
        &mut self,
        classifier: DeclarationId,
        families: Vec<ResolvedFunctionSlotFamily>,
    ) {
        assert!(
            self.classifier_header(classifier).is_some(),
            "function slots require a published classifier"
        );
        for family in &families {
            assert!(
                family.slots.len() > 1 && family.slots.iter().all(|slot| !slot.is_empty()),
                "a published function name holds several non-empty slots"
            );
            for function in family.slots.iter().flatten() {
                assert_eq!(
                    function.declared_parameters.len(),
                    function.applied_parameters.len(),
                    "a slot function applies every declared parameter"
                );
                assert_eq!(
                    function.declared_parameters.len(),
                    function.parameter_identities.len(),
                    "a slot function publishes every parameter identity"
                );
            }
        }
        assert!(
            self.function_slot_families
                .insert(classifier, families.into_boxed_slice())
                .is_none(),
            "a source classifier may publish its function slots only once"
        );
    }

    pub(super) fn function_slot_families_storage_bytes(&self) -> usize {
        self.function_slot_families
            .values()
            .map(|families| {
                std::mem::size_of::<DeclarationId>()
                    + families
                        .iter()
                        .map(|family| {
                            std::mem::size_of::<ResolvedFunctionSlotFamily>()
                                + family
                                    .slots
                                    .iter()
                                    .map(|slot| {
                                        slot.len() * std::mem::size_of::<ResolvedSlotFunction>()
                                    })
                                    .sum::<usize>()
                        })
                        .sum::<usize>()
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::features::LangFeatures;
    use crate::libraries::EmptySymbolSource;
    use crate::source::SourceInput;

    /// A slot's members, as their owners and whether the classifier declares them.
    type Slot = Vec<(String, bool)>;

    /// Each published slot of `classifier` in `source`, by function name.
    fn published_slots(source: &str, classifier: &str) -> Vec<(Box<str>, Vec<Slot>)> {
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[SourceInput::kotlin(source).with_file_stem("Slots")],
            crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let index = analysis
            .streamed
            .as_ref()
            .expect("Pass 1 must finalize")
            .module
            .index();
        let declaration = (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(classifier))
            .expect("named classifier");
        index
            .function_slot_families(declaration)
            .expect("override planning publishes the function slots")
            .iter()
            .map(|family| {
                let slots = family
                    .slots
                    .iter()
                    .map(|slot| {
                        slot.iter()
                            .map(|function| {
                                (function.owner.render(), function.declared_by_classifier)
                            })
                            .collect()
                    })
                    .collect();
                (family.name.clone(), slots)
            })
            .collect()
    }

    #[test]
    fn an_override_joins_every_declaration_it_overrides_and_an_overload_keeps_its_own_slot() {
        let slots = published_slots(
            "package sample\n\
             interface Sink<T> { fun put(value: T) }\n\
             open class Strings : Sink<String> { override fun put(value: String) {} }\n\
             class Both : Strings() { fun put(value: Any) {} }\n",
            "Both",
        );
        assert_eq!(
            slots,
            [(
                "put".into(),
                vec![
                    vec![("sample/Both".to_owned(), true)],
                    vec![
                        ("sample/Strings".to_owned(), false),
                        ("sample/Sink".to_owned(), false),
                    ],
                ],
            )]
        );
    }

    #[test]
    fn a_name_with_one_slot_is_not_published() {
        let slots = published_slots(
            "package sample\n\
             interface Sink<T> { fun put(value: T) }\n\
             class Strings : Sink<String> { override fun put(value: String) {} }\n",
            "Strings",
        );
        assert_eq!(slots, []);
    }
}
