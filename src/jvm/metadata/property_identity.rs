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

/// `Class.flags` (field 1) bit 13: the `value` modifier.
const CLASS_IS_VALUE: u64 = 1 << 13;
/// `Constructor.flags` (field 1) bit 4: a secondary constructor.
const CONSTRUCTOR_IS_SECONDARY: u64 = 1 << 4;

/// The string-table identities of a value class's components: the value parameters of its primary
/// constructor, in declaration order. Empty for any class without the `value` flag. Metadata names
/// a single underlying property for an inline value class and names nothing for a full one, but
/// both declare their components as primary-constructor properties, so the constructor is the one
/// edge every representation shares.
pub(super) fn value_class_component_name_ids(message: &[u8]) -> Vec<u64> {
    let mut flags = None;
    let mut constructors: Vec<&[u8]> = Vec::new();
    let mut pb = super::Pb::new(message);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint(),
            (8, 2) => match pb.varint().and_then(|n| pb.bytes(n as usize)) {
                Some(constructor) => constructors.push(constructor),
                None => break,
            },
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    if flags.is_none_or(|flags| flags & CLASS_IS_VALUE == 0) {
        return Vec::new();
    }
    constructors
        .into_iter()
        .find(|constructor| constructor_flags(constructor) & CONSTRUCTOR_IS_SECONDARY == 0)
        .map(parameter_name_ids)
        .unwrap_or_default()
}

fn constructor_flags(constructor: &[u8]) -> u64 {
    let mut pb = super::Pb::new(constructor);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (1, 0) => return pb.varint().unwrap_or(0),
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    0
}

fn parameter_name_ids(constructor: &[u8]) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut pb = super::Pb::new(constructor);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (2, 2) => {
                let Some(parameter) = pb.varint().and_then(|n| pb.bytes(n as usize)) else {
                    break;
                };
                let mut inner = super::Pb::new(parameter);
                while !inner.at_end() {
                    let Some(tag) = inner.varint() else { break };
                    match (tag >> 3, tag & 7) {
                        (2, 0) => {
                            ids.extend(inner.varint());
                        }
                        (_, wire) => {
                            if inner.skip(wire).is_none() {
                                break;
                            }
                        }
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
    ids
}

/// Parse the getter and setter declaration identities from a `JvmPropertySignature` extension.
/// Keeping this beside the property-name identity decoder makes the protobuf edges explicit before
/// the main decoder normalizes them into semantic property facts.
pub(super) fn parse_jvm_property_signature(body: &[u8]) -> ParsedJvmPropertySignature {
    let mut pb = super::Pb::new(body);
    let mut field = None;
    let mut synthetic_method = None;
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
            // Synthetic declaration-annotation method. Kotlin places property-targeted runtime
            // annotations here, and the metadata edge is the declaration identity for that method.
            (2, 2) => {
                if let Some(n) = pb.varint() {
                    if let Some(signature) = pb.bytes(n as usize) {
                        synthetic_method = super::parse_jvm_signature(signature);
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
        synthetic_method,
        getter,
        setter,
    }
}

/// A property's `JvmPropertySignature`: its backing field and accessors, each absent when the
/// metadata omits it.
#[derive(Clone, Copy, Default)]
pub(super) struct ParsedJvmPropertySignature {
    pub(super) field: Option<super::ParsedJvmSignature>,
    pub(super) synthetic_method: Option<super::ParsedJvmSignature>,
    pub(super) getter: Option<super::ParsedJvmSignature>,
    pub(super) setter: Option<super::ParsedJvmSignature>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn property_annotation_method_keeps_its_metadata_identity() {
        let signature = super::parse_jvm_property_signature(&[
            0x12, 0x04, // synthetic_method, four-byte message
            0x08, 0x07, // name = string-table id 7
            0x10, 0x08, // desc = string-table id 8
        ]);

        let method = signature
            .synthetic_method
            .expect("synthetic annotation method");
        assert_eq!(method.name_id, Some(7));
        assert_eq!(method.desc_id, Some(8));
    }

    #[test]
    fn underlying_property_keeps_its_string_table_identity() {
        let message = [
            0x88, 0x01, 0x07, // inline_class_underlying_property_name = d2[7]
            0x52, 0x02, 0x10, 0x09, // an unrelated Property.name = d2[9]
        ];

        assert_eq!(super::inline_underlying_property_name_id(&message), Some(7));
    }

    #[test]
    fn a_value_class_components_are_its_primary_constructor_parameters() {
        let message = [
            0x08, 0x80, 0x40, // flags: value (bit 13)
            0x42, 0x0a, // constructor, 10 bytes
            0x12, 0x03, 0x10, 0x07, 0x1a, // value_parameter name = d2[7] (type bytes elided)
            0x12, 0x02, 0x10, 0x0b, //   value_parameter name = d2[11]
            0x42, 0x04, 0x08, 0x10, 0x12, 0x00, // a secondary constructor
        ];
        assert_eq!(super::value_class_component_name_ids(&message), vec![7, 11]);
        let not_value = [0x08, 0x00, 0x42, 0x04, 0x12, 0x02, 0x10, 0x07];
        assert!(super::value_class_component_name_ids(&not_value).is_empty());
    }
}
