use super::*;
use crate::backend::interface_identity::{member_identity, property_member_identity};
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};

#[test]
fn accessor_implemented_as_a_function_keeps_its_property_slot_identity() {
    let property = KlibPublicIdSignature::classifier(crate::types::type_name("sample/Owner.value"));
    let getter = KlibDependencyCallable {
        module: "producer".into(),
        declaration: KlibDeclarationSignature::Accessor(KlibAccessorIdSignature::new(
            property.clone(),
            "physicalGetter".into(),
            11,
            0,
        )),
    };
    let setter = KlibDependencyCallable {
        module: "producer".into(),
        declaration: KlibDeclarationSignature::Accessor(KlibAccessorIdSignature::new(
            property,
            "physicalSetter".into(),
            12,
            0,
        )),
    };
    let mut exports = DependencyExportSignatures::default();
    exports.record_accessor_member_declaration(4, getter.clone(), false);
    exports.record_accessor_member_declaration(5, setter.clone(), true);
    assert_eq!(
        exports.member_slot_identity(4),
        property_member_identity(&getter, false)
    );
    assert_eq!(
        exports.member_slot_identity(5),
        property_member_identity(&getter, true)
    );
    assert_ne!(
        exports.member_slot_identity(4),
        Some(member_identity(&getter))
    );
    assert_ne!(
        exports.member_slot_identity(4),
        exports.member_slot_identity(5)
    );
    assert_eq!(exports.member_declaration(4), Some(&getter));
    assert_eq!(exports.member_declaration(5), Some(&setter));
    assert!(exports.callables().next().is_none());
    assert!(exports.interface_member(4).is_none());
}
