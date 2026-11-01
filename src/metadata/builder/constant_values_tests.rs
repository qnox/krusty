//! Complete compileTimeValue messages from the pinned official Native producer.
//! Kotlin 2.4.21 compiled tests/fixtures/unsigned_metadata_constants/UnsignedConstants.kt.
//! Zero, the high bit and the maximum cover every unsigned width beside signed contrasts.

use super::*;

#[test]
fn constants_match_complete_official_native_value_messages() {
    use crate::ir::IrConst;

    let cases: &[(&str, IrConst, &[u8])] = &[
        (
            "ubZero",
            IrConst::UByte(0),
            &[0x08, 0x00, 0x10, 0x00, 0x50, 0x01],
        ),
        (
            "ubHigh",
            IrConst::UByte(128),
            &[0x08, 0x00, 0x10, 0xff, 0x01, 0x50, 0x01],
        ),
        (
            "ubMax",
            IrConst::UByte(u8::MAX),
            &[0x08, 0x00, 0x10, 0x01, 0x50, 0x01],
        ),
        (
            "usZero",
            IrConst::UShort(0),
            &[0x08, 0x02, 0x10, 0x00, 0x50, 0x01],
        ),
        (
            "usHigh",
            IrConst::UShort(32768),
            &[0x08, 0x02, 0x10, 0xff, 0xff, 0x03, 0x50, 0x01],
        ),
        (
            "usMax",
            IrConst::UShort(u16::MAX),
            &[0x08, 0x02, 0x10, 0x01, 0x50, 0x01],
        ),
        (
            "uiZero",
            IrConst::UInt(0),
            &[0x08, 0x03, 0x10, 0x00, 0x50, 0x01],
        ),
        (
            "uiHigh",
            IrConst::UInt(2147483648),
            &[0x08, 0x03, 0x10, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x50, 0x01],
        ),
        (
            "uiMax",
            IrConst::UInt(u32::MAX),
            &[0x08, 0x03, 0x10, 0x01, 0x50, 0x01],
        ),
        (
            "ulZero",
            IrConst::ULong(0),
            &[0x08, 0x04, 0x10, 0x00, 0x50, 0x01],
        ),
        (
            "ulHigh",
            IrConst::ULong(9223372036854775808),
            &[
                0x08, 0x04, 0x10, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 0x50,
                0x01,
            ],
        ),
        (
            "ulMax",
            IrConst::ULong(u64::MAX),
            &[0x08, 0x04, 0x10, 0x01, 0x50, 0x01],
        ),
        ("signedByte", IrConst::Byte(-1), &[0x08, 0x00, 0x10, 0x01]),
        ("signedShort", IrConst::Short(-1), &[0x08, 0x02, 0x10, 0x01]),
        ("signedInt", IrConst::Int(-1), &[0x08, 0x03, 0x10, 0x01]),
        ("signedLong", IrConst::Long(-1), &[0x08, 0x04, 0x10, 0x01]),
    ];
    let mut strings = StringTable::default();
    let actual = cases
        .iter()
        .map(|(name, value, _)| (*name, constant_value_pb(&mut strings, value).into_bytes()))
        .collect::<Vec<_>>();
    let expected = cases
        .iter()
        .map(|(name, _, bytes)| (*name, bytes.to_vec()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "complete metadata value inventory");
}
