use super::{declared_unsigned_array_carrier, unsigned_array_argument, UnsignedArrayArgument};
use krusty::types::{Ty, TypeName};

const WIDTHS: [(Ty, Ty); 4] = [
    (Ty::UByte, Ty::Byte),
    (Ty::UShort, Ty::Short),
    (Ty::UInt, Ty::Int),
    (Ty::ULong, Ty::Long),
];

fn owner(element: Ty) -> TypeName {
    Ty::array(element).obj_internal().unwrap()
}

#[test]
fn every_unsigned_array_requires_its_exact_declared_signed_carrier() {
    for (unsigned, signed) in WIDTHS {
        let array = owner(unsigned);
        let carrier = Ty::array(signed);
        assert_eq!(
            declared_unsigned_array_carrier(array, Some(carrier)),
            Some(carrier)
        );
        assert_eq!(declared_unsigned_array_carrier(array, None), None);
    }
}

#[test]
fn a_changed_or_nonprimitive_carrier_does_not_authenticate_storage() {
    let primitive = [
        Ty::Byte,
        Ty::Short,
        Ty::Int,
        Ty::Long,
        Ty::Boolean,
        Ty::Char,
        Ty::Float,
        Ty::Double,
    ];
    for (unsigned, signed) in WIDTHS {
        let array = owner(unsigned);
        let expected = Ty::array(signed);
        for wrong in primitive.into_iter().filter(|ty| *ty != signed) {
            assert_eq!(
                declared_unsigned_array_carrier(array, Some(Ty::array(wrong))),
                None
            );
        }
        for wrong in [
            Ty::nullable(expected),
            Ty::array(Ty::nullable(signed)),
            Ty::array(Ty::obj("kotlin/Any")),
            Ty::array(unsigned),
            Ty::obj_args_name(expected.obj_internal().unwrap(), &[Ty::Int]),
        ] {
            assert_eq!(declared_unsigned_array_carrier(array, Some(wrong)), None);
        }
    }
    for not_unsigned in [Ty::Int, Ty::Boolean, Ty::Char, Ty::Float] {
        assert_eq!(
            declared_unsigned_array_carrier(owner(not_unsigned), Some(Ty::array(not_unsigned))),
            None
        );
    }
    assert_eq!(
        declared_unsigned_array_carrier(
            krusty::types::type_name("sample/Array"),
            Some(Ty::array(Ty::Int))
        ),
        None
    );
    assert_eq!(
        declared_unsigned_array_carrier(krusty::types::wk::array(), Some(Ty::array(Ty::Int))),
        None
    );
}

#[test]
fn every_size_frame_selects_only_its_single_size_operand() {
    for (unsigned, signed) in WIDTHS {
        let argument =
            unsigned_array_argument(owner(unsigned), Ty::array(signed), &[17], Some(&[Ty::Int]))
                .unwrap();
        assert_eq!(argument, UnsignedArrayArgument::Size(17));
    }
}

#[test]
fn a_primary_signed_carrier_selects_an_alias_preserving_view() {
    for (unsigned, signed) in WIDTHS {
        let carrier = Ty::array(signed);
        let argument =
            unsigned_array_argument(owner(unsigned), carrier, &[17], Some(&[carrier])).unwrap();
        assert_eq!(argument, UnsignedArrayArgument::Storage(17));
    }
}

#[test]
fn missing_or_changed_selected_frames_decline_before_allocation() {
    let array = owner(Ty::UInt);
    let carrier = Ty::array(Ty::Int);
    let carrier_parameters = [carrier];
    let frames: [(&[u32], Option<&[Ty]>); 8] = [
        (&[17], None),
        (&[], Some(&[Ty::Int])),
        (&[17, 19], Some(&[Ty::Int])),
        (&[17], Some(&[])),
        (&[17], Some(&[Ty::Long])),
        (&[17], Some(&[Ty::UInt])),
        (&[17], Some(&[Ty::Int, Ty::Int])),
        (&[17, 19], Some(&carrier_parameters)),
    ];
    for (args, selected) in frames {
        let error = unsigned_array_argument(array, carrier, args, selected).unwrap_err();
        assert_eq!(
            error.construct(),
            format!(
                "construction of `kotlin/UIntArray` from {} arguments",
                args.len()
            )
        );
        assert_eq!(error.origin(), None);
    }
}
