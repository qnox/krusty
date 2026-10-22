//! `SerializationPluginMetadataExtensions.propertiesNamesInProgramOrder`, the class metadata field
//! in which kotlinc's plugin lists an open or abstract serializable class's own elements.
//!
//! A subclass in another module generates its serializer against the parent's metadata alone, so
//! the parent publishes the names of its serial elements in declaration order. A sealed class's
//! subclasses all live in its own module and a final class has none, so neither writes the field;
//! nor does a class whose serializer the plugin does not generate.
//!
//! The field is read off the checked class while the target writes its metadata record
//! ([`crate::plugins::class_metadata`]); nothing is recorded for it beforehand.

use crate::ir::{IrClass, IrFile, IrGeneratedFunctionRole};
use crate::plugins::class_metadata::ClassMetadataExtensions;
use crate::types::type_name;

use super::{write_self_is_public, TRANSIENT_FQ};

/// The field number kotlinc's serialization plugin registers for the list.
pub(super) const PROPERTIES_NAMES_IN_PROGRAM_ORDER: u32 = 18000;

/// Write `class`'s own serial element names in program order, when the plugin publishes them.
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
    // Every property with a backing field is an element unless it is `@Transient`
    // (`serial_elements`).
    let transient = type_name(TRANSIENT_FQ);
    let mut elements = class
        .properties
        .iter()
        .filter(|property| property.backing_field.is_some())
        .filter(|property| !property.annotations.contains(&transient))
        .collect::<Vec<_>>();
    elements.sort_by_key(|property| property.source_order);
    for property in elements {
        out.repeated_string(PROPERTIES_NAMES_IN_PROGRAM_ORDER, &property.name);
    }
}
