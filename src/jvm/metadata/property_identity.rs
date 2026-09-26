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

/// Parse the getter and setter declaration identities from a `JvmPropertySignature` extension.
/// Keeping this beside the property-name identity decoder makes the protobuf edges explicit before
/// the main decoder normalizes them into semantic property facts.
pub(super) fn parse_jvm_property_signature(body: &[u8]) -> ParsedJvmPropertySignature {
    let mut pb = super::Pb::new(body);
    let mut field = None;
    let mut getter = None;
    let mut setter = None;
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // `JvmFieldSignature` has the same `name`/`desc` fields as a method signature.
            (1, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(signature) = pb.bytes(n as usize) {
                        field = super::parse_jvm_signature(signature);
                    }
                }
            }
            (3, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(signature) = pb.bytes(n as usize) {
                        getter = super::parse_jvm_signature(signature);
                    }
                }
            }
            (4, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(signature) = pb.bytes(n as usize) {
                        setter = super::parse_jvm_signature(signature);
                    }
                }
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    ParsedJvmPropertySignature {
        field,
        getter,
        setter,
    }
}

/// A property's `JvmPropertySignature`: its backing field and accessors, each absent when the
/// metadata omits it.
#[derive(Clone, Copy, Default)]
pub(super) struct ParsedJvmPropertySignature {
    pub(super) field: Option<super::ParsedJvmSignature>,
    pub(super) getter: Option<super::ParsedJvmSignature>,
    pub(super) setter: Option<super::ParsedJvmSignature>,
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
