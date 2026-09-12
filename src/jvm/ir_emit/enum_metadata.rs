//! Projection of checked enum entries into their Kotlin-metadata boundary records.
//!
//! An enum constant has no property declaration. Its annotations live on the physical field in
//! common IR, while Kotlin metadata records them on the enum entry. This module owns that one
//! representation mapping so the main emitter only passes normalized entry records to the metadata
//! builder.

use crate::ir::IrClass;
use crate::metadata::class_builder::EnumEntryMeta;

pub(super) fn entries(class: &IrClass) -> Vec<EnumEntryMeta<'_>> {
    class
        .enum_entries
        .iter()
        .map(|entry| EnumEntryMeta {
            name: &entry.name,
            annotations: class
                .field_annotations
                .iter()
                .find(|annotations| annotations.field == entry.name)
                .map(|annotations| &annotations.annotations),
        })
        .collect()
}
