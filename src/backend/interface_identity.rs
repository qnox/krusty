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
pub(crate) fn member_identity(member: &crate::backend::KlibDependencyCallable) -> Vec<u8> {
    let mut bytes = Vec::new();
    identity_string(&mut bytes, &member.module);
    match &member.declaration {
        crate::libraries::KlibDeclarationSignature::Public(signature) => {
            bytes.push(0);
            identity_public(&mut bytes, signature);
        }
        crate::libraries::KlibDeclarationSignature::Accessor(accessor) => {
            bytes.push(1);
            identity_public(&mut bytes, accessor.property());
            identity_string(&mut bytes, accessor.name());
            bytes.extend_from_slice(&accessor.member_id().to_le_bytes());
            bytes.extend_from_slice(&accessor.mask().to_le_bytes());
        }
    }
    bytes
}

/// The bytes that identify the getter (or, with `setter`, the setter) of one interface property
/// across modules, from the published identity of any of its accessors: the declaring module and
/// the property's public KLIB signature. Either accessor names the same property, so a module
/// that knows only the getter's identity and one that knows only the setter's agree. `None` for an
/// identity that names no accessor.
pub(crate) fn property_member_identity(
    accessor: &crate::backend::KlibDependencyCallable,
    setter: bool,
) -> Option<Vec<u8>> {
    let crate::libraries::KlibDeclarationSignature::Accessor(signature) = &accessor.declaration
    else {
        return None;
    };
    let mut bytes = Vec::new();
    identity_string(&mut bytes, &accessor.module);
    bytes.push(2);
    identity_public(&mut bytes, signature.property());
    bytes.push(u8::from(setter));
    Some(bytes)
}

fn identity_string(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
}

fn identity_public(
    bytes: &mut Vec<u8>,
    signature: &crate::metadata::id_signature::KlibPublicIdSignature,
) {
    for segments in [
        signature.package().segments(),
        signature.declaration().segments(),
    ] {
        bytes.extend_from_slice(&(segments.len() as u32).to_le_bytes());
        for segment in segments {
            identity_string(bytes, segment);
        }
    }
    match signature.member_id() {
        Some(id) => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_le_bytes());
        }
        None => bytes.push(0),
    }
    bytes.extend_from_slice(&signature.mask().to_le_bytes());
}
