//! Declaration-identity edges encoded directly on Kotlin metadata properties.

/// The string-table identity in `Class.inline_class_underlying_property_name` (field 17).
///
/// Keeping the numeric identity until the corresponding `Property.name` field is decoded avoids a
/// later spelling-based join between two declarations the protobuf already relates directly.
pub(super) fn inline_underlying_property_name_id(message: &[u8]) -> Option<u64> {
    let mut pb = super::Pb::new(message);
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (17, 0) => return pb.varint(),
            (_, wire) => pb.skip(wire)?,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn underlying_property_keeps_its_string_table_identity() {
        let message = [
            0x88, 0x01, 0x07, // inline_class_underlying_property_name = d2[7]
            0x52, 0x02, 0x10, 0x09, // an unrelated Property.name = d2[9]
        ];

        assert_eq!(super::inline_underlying_property_name_id(&message), Some(7));
    }
}
