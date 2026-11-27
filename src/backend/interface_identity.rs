//! The identity of an interface member across source modules.
//!
//! A member number means something only inside the module that numbered it
//! ([`super::class_tables::module_interfaces`]), so a class of a dependent module implementing a
//! dependency's interface cannot name the number. Both modules instead compute the same bytes from
//! the member's published KLIB declaration identity: the module that declares the member publishes
//! them beside its number, and a dependent module names its implementation by them. Every target
//! that joins implementations to calls across modules uses these bytes, never a spelling.

/// The bytes that identify one interface function across modules: its declaring module and its
/// public KLIB signature, in an injective encoding (each string length-prefixed). Both the module
/// that declares the member and every module implementing it compute it from the same published
/// identity, never from a spelling.
pub fn member_identity(member: &crate::libraries::KlibDependencyCallable) -> Vec<u8> {
    let mut bytes = Vec::new();
    identity_string(&mut bytes, &member.module);
    match &member.declaration {
        crate::libraries::KlibDeclarationSignature::Public(signature) => {
            bytes.push(0);
            signature.write_identity(&mut bytes);
        }
        crate::libraries::KlibDeclarationSignature::Accessor(accessor) => {
            bytes.push(1);
            accessor.property().write_identity(&mut bytes);
            identity_string(&mut bytes, accessor.name());
            bytes.extend_from_slice(&accessor.member_id().to_le_bytes());
            bytes.extend_from_slice(&accessor.mask().to_le_bytes());
        }
        &crate::libraries::KlibDeclarationSignature::FileLocal { file, slot } => {
            bytes.push(2);
            bytes.extend_from_slice(&file.to_le_bytes());
            bytes.extend_from_slice(&slot.to_le_bytes());
        }
    }
    bytes
}

/// Exact identity of a classifier, independent of any callable it declares or implements.
pub fn classifier_identity(classifier: &crate::libraries::KlibClassIdentity) -> Vec<u8> {
    let mut bytes = Vec::new();
    identity_string(&mut bytes, &classifier.module);
    classifier.signature.write_identity(&mut bytes);
    bytes
}

/// The bytes that identify the getter (or, with `setter`, the setter) of one interface property
/// across modules, from the published identity of any of its accessors: the declaring module and
/// the property's public KLIB signature. Either accessor names the same property, so a module
/// that knows only the getter's identity and one that knows only the setter's agree. `None` for an
/// identity that names no accessor.
pub fn property_member_identity(
    accessor: &crate::libraries::KlibDependencyCallable,
    setter: bool,
) -> Option<Vec<u8>> {
    let crate::libraries::KlibDeclarationSignature::Accessor(signature) = &accessor.declaration
    else {
        return None;
    };
    let mut bytes = Vec::new();
    identity_string(&mut bytes, &accessor.module);
    bytes.push(2);
    signature.property().write_identity(&mut bytes);
    bytes.push(u8::from(setter));
    Some(bytes)
}

fn identity_string(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_identity_includes_exact_module_and_structural_signature() {
        use crate::libraries::KlibClassIdentity;
        use crate::metadata::id_signature::KlibPublicIdSignature;
        use crate::types::{type_name, type_name_nested_child};
        let first = KlibClassIdentity {
            module: "first".into(),
            signature: KlibPublicIdSignature::classifier(type_name("sample/Contract")),
        };
        let mut second = first.clone();
        second.module = "second".into();
        assert_ne!(classifier_identity(&first), classifier_identity(&second));
        let nested = KlibClassIdentity {
            module: first.module.clone(),
            signature: KlibPublicIdSignature::classifier(type_name_nested_child(
                type_name("sample/Owner"),
                "Contract",
            )),
        };
        let top_level = KlibClassIdentity {
            module: first.module.clone(),
            // A top-level Contract in package sample.Owner has the same dotted display as
            // nested sample.Owner.Contract, but different package/declaration boundaries.
            signature: KlibPublicIdSignature::classifier(type_name("sample/Owner/Contract")),
        };
        assert_eq!(
            nested.signature.declaration().segments(),
            &["Owner".to_owned(), "Contract".to_owned()]
        );
        assert_eq!(
            top_level.signature.declaration().segments(),
            &["Contract".to_owned()]
        );
        assert_eq!(
            nested.signature.package().segments(),
            &["sample".to_owned()]
        );
        assert_eq!(
            top_level.signature.package().segments(),
            &["sample".to_owned(), "Owner".to_owned()]
        );
        assert_ne!(
            classifier_identity(&nested),
            classifier_identity(&top_level)
        );
        assert_eq!(
            classifier_identity(&first),
            classifier_identity(&first.clone())
        );
    }
}
