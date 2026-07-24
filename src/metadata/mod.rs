//! Kotlin @Metadata emission (protobuf payload + encoding). WIP — Phase 4b.

use protobuf::Pb;

pub mod builder;
pub mod class_builder;
pub mod encoding;
pub mod module;
pub mod protobuf;

pub(crate) fn serialize_string_table_types(records: &[Pb]) -> Pb {
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

        let encoded = serialize_string_table_types(&[
            plain.clone(),
            plain,
            predefined,
            operation.clone(),
            operation,
        ]);

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
