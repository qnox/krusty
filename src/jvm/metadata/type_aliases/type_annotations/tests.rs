//! Full scalar value messages from the official Native 2.4.21 constant fixture.

use super::*;
use crate::metadata::protobuf::Pb;

fn decode(bytes: &[u8]) -> Option<AnnotationValue> {
    let mut argument = Pb::new();
    argument.field_varint(1, 0);
    argument.field_bytes(2, bytes);
    let mut annotation = Pb::new();
    annotation.field_varint(1, 0);
    annotation.field_message(2, &argument);
    let mut ty = Pb::new();
    ty.field_message(100, &annotation);
    let parsed = crate::metadata::decode::parse_type_node(ty.as_bytes())?;
    let value = &parsed.annotations.first()?.arguments().first()?.value;
    bind_value(value, &[], &[])
}

#[test]
fn complete_official_value_inventory_preserves_unsigned_identity() {
    use AnnotationValue::*;
    let cases: &[(&str, &[u8], AnnotationValue)] = &[
        ("ubZero", &[0x08, 0x00, 0x10, 0x00, 0x50, 0x01], UByte(0)),
        (
            "ubHigh",
            &[0x08, 0x00, 0x10, 0xff, 0x01, 0x50, 0x01],
            UByte(128),
        ),
        (
            "ubMax",
            &[0x08, 0x00, 0x10, 0x01, 0x50, 0x01],
            UByte(u8::MAX),
        ),
        ("usZero", &[0x08, 0x02, 0x10, 0x00, 0x50, 0x01], UShort(0)),
        (
            "usHigh",
            &[0x08, 0x02, 0x10, 0xff, 0xff, 0x03, 0x50, 0x01],
            UShort(32768),
        ),
        (
            "usMax",
            &[0x08, 0x02, 0x10, 0x01, 0x50, 0x01],
            UShort(u16::MAX),
        ),
        ("uiZero", &[0x08, 0x03, 0x10, 0x00, 0x50, 0x01], UInt(0)),
        (
            "uiHigh",
            &[0x08, 0x03, 0x10, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x50, 0x01],
            UInt(2147483648),
        ),
        (
            "uiMax",
            &[0x08, 0x03, 0x10, 0x01, 0x50, 0x01],
            UInt(u32::MAX),
        ),
        ("ulZero", &[0x08, 0x04, 0x10, 0x00, 0x50, 0x01], ULong(0)),
        (
            "ulHigh",
            &[
                0x08, 0x04, 0x10, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 0x50,
                0x01,
            ],
            ULong(9223372036854775808),
        ),
        (
            "ulMax",
            &[0x08, 0x04, 0x10, 0x01, 0x50, 0x01],
            ULong(u64::MAX),
        ),
        ("signedByte", &[0x08, 0x00, 0x10, 0x01], Byte(-1)),
        ("signedShort", &[0x08, 0x02, 0x10, 0x01], Short(-1)),
        ("signedInt", &[0x08, 0x03, 0x10, 0x01], Int(-1)),
        ("signedLong", &[0x08, 0x04, 0x10, 0x01], Long(-1)),
    ];
    let actual = cases
        .iter()
        .map(|(name, bytes, _)| (*name, decode(bytes)))
        .collect::<Vec<_>>();
    let expected = cases
        .iter()
        .map(|(name, _, value)| (*name, Some(value.clone())))
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "complete official metadata value inventory"
    );
}

#[test]
fn unknown_flags_and_invalid_unsigned_carriers_decline() {
    let invalid: &[&[u8]] = &[
        &[0x08, 0x00, 0x10, 0x01, 0x50, 0x02],       // unknown flag
        &[0x08, 0x00, 0x10, 0x01, 0x50, 0x03],       // unsigned plus an unknown flag
        &[0x08, 0x01, 0x10, 0x00, 0x50, 0x01],       // unsigned char
        &[0x08, 0x0c, 0x50, 0x01],                   // unsigned array wrapper
        &[0x08, 0x00, 0x10, 0x80, 0x02, 0x50, 0x01], // BYTE carrier +128 is out of range
    ];
    assert_eq!(
        invalid
            .iter()
            .map(|bytes| decode(bytes))
            .collect::<Vec<_>>(),
        vec![None; invalid.len()]
    );
}
