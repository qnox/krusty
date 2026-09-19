//! Carrier-independent wire records for Kotlin type and value parameters.
//!
//! These records retain numeric ids and nested type messages exactly as encoded. Semantic adapters
//! resolve them against their carrier's string, qualified-name, and type-table domains.

use super::{packed_varints, Pb};

#[derive(Clone, Copy)]
pub(crate) enum ParsedVariance {
    In,
    Out,
    Invariant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParameterDecodeError {
    MalformedWire,
    MissingField(&'static str),
    InvalidVariance(u64),
}

pub(crate) struct ParsedTypeParam {
    pub(crate) id: u64,
    pub(crate) name_id: u64,
    pub(crate) reified: bool,
    pub(crate) upper_bound_bodies: Vec<Vec<u8>>,
    /// `TypeParameter.upper_bound_id` (field 6), the table-backed form used by builtins/KLIB.
    pub(crate) upper_bound_ids: Vec<u64>,
    pub(crate) variance: ParsedVariance,
    /// Raw core/builtins `Annotation` messages declared on this type parameter.
    pub(crate) annotation_bodies: Vec<Vec<u8>>,
}

pub(crate) fn parse_type_param(body: &[u8]) -> Result<ParsedTypeParam, ParameterDecodeError> {
    let mut protobuf = Pb::new(body);
    let mut id = None;
    let mut name = None;
    let mut upper_bound_bodies = Vec::new();
    let mut upper_bound_ids = Vec::new();
    let mut reified = false;
    let mut variance = ParsedVariance::Invariant;
    let mut annotation_bodies = Vec::new();
    while !protobuf.at_end() {
        let tag = protobuf
            .varint()
            .ok_or(ParameterDecodeError::MalformedWire)?;
        match (tag >> 3, tag & 7) {
            (1, 0) => {
                id = Some(
                    protobuf
                        .varint()
                        .ok_or(ParameterDecodeError::MalformedWire)?,
                )
            }
            (2, 0) => {
                name = Some(
                    protobuf
                        .varint()
                        .ok_or(ParameterDecodeError::MalformedWire)?,
                )
            }
            (3, 0) => {
                reified = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    != 0
            }
            (4, 0) => {
                variance = match protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                {
                    0 => ParsedVariance::In,
                    1 => ParsedVariance::Out,
                    2 => ParsedVariance::Invariant,
                    value => return Err(ParameterDecodeError::InvalidVariance(value)),
                }
            }
            (5, 2) => {
                let length = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    as usize;
                upper_bound_bodies.push(
                    protobuf
                        .bytes(length)
                        .ok_or(ParameterDecodeError::MalformedWire)?
                        .to_vec(),
                );
            }
            (6, 0) => upper_bound_ids.push(
                protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?,
            ),
            (6, 2) => {
                let length = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    as usize;
                let packed = protobuf
                    .bytes(length)
                    .ok_or(ParameterDecodeError::MalformedWire)?;
                upper_bound_ids
                    .extend(packed_varints(packed).ok_or(ParameterDecodeError::MalformedWire)?);
            }
            // `ProtoBuf.TypeParameter.annotation` and the builtins/KLIB extension encodings.
            (100, 2) | (150, 2) | (170, 2) => {
                let length = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    as usize;
                annotation_bodies.push(
                    protobuf
                        .bytes(length)
                        .ok_or(ParameterDecodeError::MalformedWire)?
                        .to_vec(),
                );
            }
            (_, wire) => protobuf
                .skip(wire)
                .ok_or(ParameterDecodeError::MalformedWire)?,
        }
    }
    Ok(ParsedTypeParam {
        id: id.ok_or(ParameterDecodeError::MissingField("id"))?,
        name_id: name.ok_or(ParameterDecodeError::MissingField("name"))?,
        reified,
        upper_bound_bodies,
        upper_bound_ids,
        variance,
        annotation_bodies,
    })
}

/// One source `ValueParameter`, before its ids and nested type messages are semantically resolved.
pub(crate) struct ParsedValueParam {
    pub(crate) name_id: u64,
    pub(crate) flags: u64,
    pub(crate) type_body: Option<Vec<u8>>,
    pub(crate) type_id: Option<u64>,
    pub(crate) vararg_elem_body: Option<Vec<u8>>,
    pub(crate) vararg_elem_id: Option<u64>,
    pub(crate) equality_bound_body: Option<Vec<u8>>,
    pub(crate) equality_bound_id: Option<u64>,
}

pub(crate) fn parse_value_parameter(body: &[u8]) -> Result<ParsedValueParam, ParameterDecodeError> {
    let mut protobuf = Pb::new(body);
    let mut name_id = None;
    let mut flags = 0u64;
    let mut type_body = None;
    let mut type_id = None;
    let mut vararg_elem_body = None;
    let mut vararg_elem_id = None;
    let mut equality_bound_body = None;
    let mut equality_bound_id = None;
    while !protobuf.at_end() {
        let tag = protobuf
            .varint()
            .ok_or(ParameterDecodeError::MalformedWire)?;
        match (tag >> 3, tag & 7) {
            (1, 0) => {
                flags = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
            }
            (2, 0) => name_id = protobuf.varint(),
            (3, 2) => {
                let length = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    as usize;
                type_body = Some(
                    protobuf
                        .bytes(length)
                        .ok_or(ParameterDecodeError::MalformedWire)?
                        .to_vec(),
                );
            }
            (4, 2) => {
                let length = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    as usize;
                vararg_elem_body = Some(
                    protobuf
                        .bytes(length)
                        .ok_or(ParameterDecodeError::MalformedWire)?
                        .to_vec(),
                );
            }
            (5, 0) => {
                type_id = Some(
                    protobuf
                        .varint()
                        .ok_or(ParameterDecodeError::MalformedWire)?,
                )
            }
            (6, 0) => {
                vararg_elem_id = Some(
                    protobuf
                        .varint()
                        .ok_or(ParameterDecodeError::MalformedWire)?,
                )
            }
            (9, 2) => {
                let length = protobuf
                    .varint()
                    .ok_or(ParameterDecodeError::MalformedWire)?
                    as usize;
                equality_bound_body = Some(
                    protobuf
                        .bytes(length)
                        .ok_or(ParameterDecodeError::MalformedWire)?
                        .to_vec(),
                );
            }
            (10, 0) => {
                equality_bound_id = Some(
                    protobuf
                        .varint()
                        .ok_or(ParameterDecodeError::MalformedWire)?,
                )
            }
            (_, wire) => protobuf
                .skip(wire)
                .ok_or(ParameterDecodeError::MalformedWire)?,
        }
    }
    Ok(ParsedValueParam {
        name_id: name_id.ok_or(ParameterDecodeError::MissingField("name"))?,
        flags,
        type_body,
        type_id,
        vararg_elem_body,
        vararg_elem_id,
        equality_bound_body,
        equality_bound_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_parameter_rejects_malformed_packed_bounds_and_unknown_variance() {
        assert!(matches!(
            parse_type_param(&[0x08, 0x00, 0x10, 0x00, 0x32, 0x01, 0x80]),
            Err(ParameterDecodeError::MalformedWire)
        ));
        assert!(matches!(
            parse_type_param(&[0x08, 0x00, 0x10, 0x00, 0x20, 0x03]),
            Err(ParameterDecodeError::InvalidVariance(3))
        ));
    }

    #[test]
    fn value_parameter_retains_distinct_source_flags() {
        let crossinline = parse_value_parameter(&[0x08, 0x04, 0x10, 0x00]).unwrap();
        let noinline = parse_value_parameter(&[0x08, 0x08, 0x10, 0x00]).unwrap();
        assert_eq!(crossinline.flags, 1 << 2);
        assert_eq!(noinline.flags, 1 << 3);
    }

    #[test]
    fn value_parameter_rejects_truncated_scalar_fields() {
        for field in [0x28, 0x30, 0x50] {
            assert!(matches!(
                parse_value_parameter(&[0x10, 0x00, field, 0x80]),
                Err(ParameterDecodeError::MalformedWire)
            ));
        }
    }
}
