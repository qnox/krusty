use super::*;
use krusty::types::type_name;

fn declared(underlying: Ty) -> DeclaredValueClass {
    DeclaredValueClass {
        property: "data".into(),
        underlying,
        type_parameters: Box::new([]),
    }
}

#[test]
fn a_value_class_sharing_its_underlying_scalar_carrier_is_that_scalar() {
    assert_eq!(
        scalar_representation(type_name("kotlin/UInt"), &declared(Ty::Int)),
        Ok(Some(ValType::I32))
    );
    assert_eq!(
        scalar_representation(type_name("kotlin/ULong"), &declared(Ty::Long)),
        Ok(Some(ValType::I64))
    );
}

#[test]
fn a_value_class_carried_as_an_object_is_not_its_underlying_scalar() {
    assert_eq!(
        scalar_representation(type_name("sample/Meters"), &declared(Ty::Int)),
        Ok(None)
    );
}

#[test]
fn a_declaration_whose_underlying_carrier_differs_is_not_the_class_scalar() {
    assert_eq!(
        scalar_representation(type_name("kotlin/UInt"), &declared(Ty::Long)),
        Ok(None)
    );
}

#[test]
fn runtime_array_views_retain_their_own_representation() {
    for (owner, underlying) in [
        ("kotlin/UByteArray", Ty::obj("kotlin/ByteArray")),
        ("kotlin/UShortArray", Ty::obj("kotlin/ShortArray")),
        ("kotlin/UIntArray", Ty::obj("kotlin/IntArray")),
        ("kotlin/ULongArray", Ty::obj("kotlin/LongArray")),
    ] {
        assert_eq!(
            scalar_representation(type_name(owner), &declared(underlying)),
            Ok(Some(REFERENCE)),
        );
    }
    assert_eq!(
        scalar_representation(
            type_name("sample/ArrayHolder"),
            &declared(Ty::obj("kotlin/ByteArray"))
        ),
        Ok(None),
    );
}

#[test]
fn an_unsigned_class_answers_its_storage_getter_at_the_signed_value_it_wraps() {
    use krusty::backend::DependencyGetterSignature;
    let getter = |owner: &str, result: Ty| DependencyGetterSignature {
        owner: type_name(owner),
        name: "data",
        parameters: &[],
        result,
    };
    for (owner, wrapped) in [
        ("kotlin/UByte", Ty::Byte),
        ("kotlin/UShort", Ty::Short),
        ("kotlin/UInt", Ty::Int),
        ("kotlin/ULong", Ty::Long),
    ] {
        assert!(realizes_unsigned_storage_getter(getter(owner, wrapped)));
        assert!(!realizes_unsigned_storage_getter(getter(owner, Ty::Double)));
    }
    assert!(!realizes_unsigned_storage_getter(getter(
        "sample/Meters",
        Ty::Int
    )));
}
