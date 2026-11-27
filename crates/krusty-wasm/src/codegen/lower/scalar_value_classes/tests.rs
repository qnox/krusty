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
fn a_scalar_value_class_answers_the_storage_getter_its_declaration_names() {
    use krusty::backend::DependencyGetterSignature;
    let getter = |owner: &str, result: Ty, declaration: Option<&'static DeclaredValueClass>| {
        DependencyGetterSignature {
            owner: type_name(owner),
            name: "data",
            parameters: &[],
            result,
            value_declaration: declaration,
        }
    };
    let int: &'static DeclaredValueClass = Box::leak(Box::new(declared(Ty::Int)));
    let long: &'static DeclaredValueClass = Box::leak(Box::new(declared(Ty::Long)));
    let bytes: &'static DeclaredValueClass =
        Box::leak(Box::new(declared(Ty::obj("kotlin/ByteArray"))));
    assert!(realizes_scalar_value_storage_getter(getter(
        "kotlin/UInt",
        Ty::Int,
        Some(int)
    )));
    assert!(realizes_scalar_value_storage_getter(getter(
        "kotlin/ULong",
        Ty::Long,
        Some(long)
    )));
    // No declaration naming this property, a different result, a class carried as an object, or
    // a class carried as an array view each leave the read to the library body.
    assert!(!realizes_scalar_value_storage_getter(getter(
        "kotlin/UInt",
        Ty::Int,
        None
    )));
    assert!(!realizes_scalar_value_storage_getter(getter(
        "kotlin/UInt",
        Ty::Long,
        Some(int)
    )));
    assert!(!realizes_scalar_value_storage_getter(getter(
        "sample/Meters",
        Ty::Int,
        Some(int)
    )));
    assert!(!realizes_scalar_value_storage_getter(getter(
        "kotlin/UByteArray",
        Ty::obj("kotlin/ByteArray"),
        Some(bytes)
    )));
}
