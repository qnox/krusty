use super::*;
use crate::libraries::{LibConst, LibraryConst};

#[test]
fn unsigned_annotation_constant_magnitudes_authenticate_the_source_width() {
    let cases = [
        (Ty::UByte, LibConst::Int(-1), Some(255)),
        (Ty::UByte, LibConst::Int(128), Some(128)),
        (Ty::UShort, LibConst::Int(-32768), Some(32768)),
        (Ty::UInt, LibConst::Int(-1), Some(4294967295)),
        (
            Ty::ULong,
            LibConst::Long(i64::MIN),
            Some(9223372036854775808),
        ),
        (Ty::ULong, LibConst::Long(-1), Some(u64::MAX)),
        (Ty::Int, LibConst::Int(-1), None),
        (Ty::UByte, LibConst::Int(256), None),
        (Ty::UShort, LibConst::Int(-32769), None),
        (Ty::UInt, LibConst::Long(4294967296), None),
        (Ty::Byte, LibConst::Int(255), None),
        (Ty::Int, LibConst::Long(4294967295), None),
        (Ty::String, LibConst::Int(1), None),
    ];
    let actual = cases
        .iter()
        .map(|(ty, value, _)| {
            unsigned_constant_magnitude(&LibraryConst {
                ty: *ty,
                value: value.clone(),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        cases
            .iter()
            .map(|(_, _, expected)| *expected)
            .collect::<Vec<_>>()
    );
}
