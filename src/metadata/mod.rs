//! Kotlin @Metadata emission (protobuf payload + encoding). WIP — Phase 4b.

use protobuf::Pb;

pub mod builder;
pub mod class_builder;
pub mod encoding;
pub mod module;
pub mod protobuf;
mod type_encoder;

/// Canonical `ProtoBuf.Property.flags` layout shared by metadata readers and writers. Property flags
/// include the declaration's two-bit MEMBER_KIND after MODALITY, so property-specific bits must not be
/// copied from the shorter Function/Class prefix. Keeping the raw schema facts here prevents the JVM
/// decoder, class encoder, and package encoder from drifting into separate numeric interpretations.
pub(crate) mod property_flags {
    /// Public, final property with a default getter; protobuf omits field 11 at this value.
    pub const DEFAULT: u64 = 518;
    pub const VISIBILITY_MASK: u64 = 0b1110;
    pub const IS_VAR: u64 = 1 << 8;
    pub const HAS_SETTER: u64 = 1 << 10;
    pub const IS_CONST: u64 = 1 << 11;
    pub const HAS_CONSTANT: u64 = 1 << 13;
    pub const MODALITY_ABSTRACT: u64 = 1 << 5;
}

pub(crate) fn serialize_string_table_types(records: &[Pb], local_names: &[u32]) -> Pb {
    let mut out = Pb::new();
    let mut i = 0;
    while i < records.len() {
        if !records[i].is_empty() {
            out.repeated_message(1, &records[i]);
            i += 1;
            continue;
        }

        let mut end = i + 1;
        while end < records.len() && records[end].is_empty() {
            end += 1;
        }
        let mut record = Pb::new();
        if end - i > 1 {
            record.field_varint(1, (end - i) as u64);
        }
        out.repeated_message(1, &record);
        i = end;
    }
    // `StringTableTypes.localName` (packed field 5): the indices whose strings are LOCAL class
    // names — raw internal names used as class ids verbatim (kotlinc's anonymous-class encoding).
    if !local_names.is_empty() {
        let mut packed = Pb::new();
        for &index in local_names {
            packed.varint(index as u64);
        }
        out.field_bytes(5, packed.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_table_types_merge_only_plain_record_runs() {
        let plain = Pb::new();
        let mut predefined = Pb::new();
        predefined.field_varint(2, 8);
        let mut operation = Pb::new();
        operation.field_varint(3, 2);

        let encoded = serialize_string_table_types(
            &[
                plain.clone(),
                plain,
                predefined,
                operation.clone(),
                operation,
            ],
            &[],
        );

        assert_eq!(
            encoded.as_bytes(),
            &[
                0x0a, 0x02, 0x08, 0x02, // two plain records
                0x0a, 0x02, 0x10, 0x08, // predefined index
                0x0a, 0x02, 0x18, 0x02, // operation
                0x0a, 0x02, 0x18, 0x02, // identical operation stays separate
            ]
        );
    }
}
