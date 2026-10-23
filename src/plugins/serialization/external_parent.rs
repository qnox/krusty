//! Serializable superclasses compiled apart from the file being lowered: in a dependency, as
//! kotlinc's plugin reads them from the superclass's metadata, or in another file of the module,
//! as the module's stable declarations describe them.
//!
//! A subclass's generated serializer inherits its superclass's elements: the superclass's
//! `write$Self` writes them and its deserialization constructor stores them, and the subclass's
//! descriptor, `childSerializers` and `deserialize` list them first. For a superclass this file
//! declares, the plugin reads the elements off its checked IR ([`super::serial_elements`]); for
//! one it does not, the provider publishes the same facts as a
//! [`SerializableClassElements`] record.
//!
//! A superclass whose generated serializer the provider cannot describe exactly is recorded as
//! unpublished, and a subclass of it declines rather than call members that may not exist.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::IrFile;
use crate::types::{type_name, ClassifierFactSource, SerializableClassElements, Ty, TypeName};

use super::SERIALIZABLE_FQ;

/// What the provider published for a serializable superclass this file does not declare.
pub type ExternalSerializableParents = HashMap<TypeName, Option<SerializableClassElements>>;

/// Every serializable superclass a class of this file extends without declaring it, transitively
/// up its chain of serializable superclasses, with the record its provider published for it.
pub(crate) fn external_serializable_parents(
    ir: &IrFile,
    classifiers: &dyn ClassifierFactSource,
) -> ExternalSerializableParents {
    let serializable = type_name(SERIALIZABLE_FQ);
    let declared = ir
        .classes
        .iter()
        .map(|class| class.fq_name_id())
        .collect::<HashSet<_>>();
    let mut pending = ir
        .classes
        .iter()
        .filter(|class| {
            class
                .applied_annotations
                .applications()
                .any(|annotation| annotation.internal == serializable)
        })
        .map(|class| class.superclass)
        .filter(|superclass| !declared.contains(superclass))
        .collect::<Vec<_>>();
    let mut parents = ExternalSerializableParents::default();
    while let Some(classifier) = pending.pop() {
        if parents.contains_key(&classifier) {
            continue;
        }
        // Only a superclass whose serializer the plugin generated contributes elements; one with
        // `@Serializable(with = …)` or none at all is serialized by the subclass alone.
        let generated = classifiers
            .classifier_annotations(classifier)
            .is_some_and(|annotations| {
                annotations.iter().any(|annotation| {
                    annotation.annotation == serializable
                        && !annotation.arguments.iter().any(|(name, _)| name == "with")
                })
            });
        if !generated {
            continue;
        }
        let record = classifiers.serializable_class_elements(classifier);
        if let Some(superclass) = record
            .as_ref()
            .and_then(|record| record.superclass)
            .and_then(|superclass| superclass.kotlin_class_internal())
        {
            pending.push(superclass);
        }
        parents.insert(classifier, record);
    }
    parents
}

/// The types of every element the published superclasses declare, whose serializers a subclass's
/// generated members name.
pub(crate) fn inherited_element_types(parents: &ExternalSerializableParents) -> Vec<Ty> {
    parents
        .values()
        .flatten()
        .flat_map(|record| record.elements.iter().map(|element| element.ty))
        .collect()
}
