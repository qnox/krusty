//! `SerializationPluginMetadataExtensions.propertiesNamesInProgramOrder`, the class metadata field
//! in which kotlinc's plugin lists an open or abstract serializable class's serial elements.
//!
//! A subclass in another module generates its serializer against the parent's metadata alone, so
//! the parent publishes the names of its serial elements in wire order: the elements it inherits
//! from its serializable superclasses first, then its own in declaration order. A sealed class's
//! subclasses all live in its own module and a final class has none, so neither writes the field;
//! nor does a class whose serializer the plugin does not generate.
//!
//! The field is read off the checked class while the target writes its metadata record
//! ([`crate::plugins::class_metadata`]); nothing is recorded for it beforehand.

use super::write_self_is_public;
use crate::ir::{IrClass, IrFile, IrGeneratedFunctionRole, IrSecondaryConstructorRole};
use crate::plugins::class_metadata::ClassMetadataExtensions;

/// The field number kotlinc's serialization plugin registers for the list.
pub(super) const PROPERTIES_NAMES_IN_PROGRAM_ORDER: u32 = 18000;

/// Write `class`'s serial element names in wire order, when the plugin publishes them.
pub(super) fn write(ir: &IrFile, class: &IrClass, out: &mut ClassMetadataExtensions<'_>) {
    if !write_self_is_public(class) || class.is_sealed {
        return;
    }
    // The plugin generated this class's serializer exactly when it gave the class `write$Self`.
    if ir
        .generated_function(
            class.fq_name_id(),
            IrGeneratedFunctionRole::SerializationWriteSelf,
        )
        .is_none()
    {
        return;
    }
    // The deserialization constructor takes every element, inherited ones first, between its
    // `seen` masks and its marker, each parameter named after its element.
    let Some(ordinal) = ir.generated_secondary_constructor_by_owner(
        class.fq_name_id(),
        IrSecondaryConstructorRole::SerializationDeserialization,
    ) else {
        return;
    };
    let parameters = &class.secondary_ctors[ordinal as usize].named_params;
    let elements = element_count(parameters.len());
    let masks = parameters.len() - elements - 1;
    for (name, _) in &parameters[masks..masks + elements] {
        out.repeated_string(PROPERTIES_NAMES_IN_PROGRAM_ORDER, name);
    }
}

/// How many elements a deserialization constructor of `parameters` parameters takes: one `seen`
/// mask per 32 elements, plus one, and the marker take the rest.
fn element_count(parameters: usize) -> usize {
    (0..parameters)
        .find(|elements| elements + elements / 32 + 2 == parameters)
        .expect("a deserialization constructor takes its masks, its elements and its marker")
}
