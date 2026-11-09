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
