//! A member's identity apart from the class that declares it.
//!
//! kotlinc's member ids hash a member's own mangled signature, never its owner, so the same member
//! keeps its last declaration segment, member id and mask in every class. A member of a class
//! private to its file has no public signature: its identity is the one of the declarations it
//! overrides, which a fake override of a subclass overrides alike.

use super::super::KlibDeclarationBodies;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};
use crate::metadata::klib_ir::KlibIrSignature;

/// The owner-independent identity of a member function or property accessor.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum MemberKey {
    Function(PublicMemberKey),
    Accessor {
        property: PublicMemberKey,
        name: String,
        member_id: u64,
        mask: u64,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct PublicMemberKey {
    member: String,
    member_id: Option<u64>,
    mask: u64,
}

impl PublicMemberKey {
    fn of(signature: &KlibPublicIdSignature) -> Option<Self> {
        Some(Self {
            member: signature.declaration().segments().last()?.clone(),
            member_id: signature.member_id(),
            mask: signature.mask(),
        })
    }
}

/// The key a public member or accessor signature carries; none for a file-local one.
fn signature_key(signature: &KlibIrSignature) -> Option<MemberKey> {
    match signature {
        KlibIrSignature::Public(signature) => {
            PublicMemberKey::of(signature).map(MemberKey::Function)
        }
        KlibIrSignature::Accessor(accessor) => Some(MemberKey::Accessor {
            property: PublicMemberKey::of(accessor.property())?,
            name: accessor.name().to_owned(),
            member_id: accessor.member_id(),
            mask: accessor.mask(),
        }),
        KlibIrSignature::FileLocal { .. } | KlibIrSignature::TypeParameter { .. } => None,
    }
}

/// The exact identity of the member `signature` names in `module`: a file-local one is a slot of
/// that module's own files; a public one is the declaration its producer serializes.
pub(super) fn member_identity(
    bodies: &KlibDeclarationBodies,
    module: &str,
    signature: &KlibIrSignature,
) -> Result<Option<KlibDependencyCallable>, &'static str> {
    let declaration = match signature {
        KlibIrSignature::FileLocal { file, slot } => {
            return Ok(Some(KlibDependencyCallable {
                module: module.to_owned(),
                declaration: KlibDeclarationSignature::FileLocal {
                    file: *file,
                    slot: *slot,
                },
            }))
        }
        KlibIrSignature::Public(signature) => KlibDeclarationSignature::Public(signature.clone()),
        KlibIrSignature::Accessor(signature) => {
            KlibDeclarationSignature::Accessor(signature.clone())
        }
        // A type parameter is no member.
        KlibIrSignature::TypeParameter { .. } => return Ok(None),
    };
    match bodies.callee_origin(module, &declaration) {
        super::KlibCalleeOrigin::Exact(identity) => Ok(Some(identity)),
        super::KlibCalleeOrigin::Missing => Ok(None),
        super::KlibCalleeOrigin::Ambiguous => Err("a member is serialized twice"),
    }
}

/// The serialized member function `identity`: a member of a class private to its file is a slot
/// of its module's own files.
pub(super) fn member_function<'a>(
    bodies: &'a KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Option<(&'a KlibIrArena, &'a KlibIrFunction)> {
    match &identity.declaration {
        KlibDeclarationSignature::FileLocal { file, slot } => bodies.module_local_function(
            &identity.module,
            &KlibIrSignature::FileLocal {
                file: *file,
                slot: *slot,
            },
        ),
        _ => bodies.function(identity),
    }
}

/// The key of the member function `function` of a class in `module`: its own signature's, or, for
/// a file-local member, the key of a declaration it overrides. A file-local member that overrides
/// nothing is not a member any supertype declares, so it has none.
pub(super) fn function_key(
    bodies: &KlibDeclarationBodies,
    module: &str,
    function: &KlibIrFunction,
) -> Option<MemberKey> {
    let mut pending = vec![function];
    // Each step follows an override edge to a declaration of a supertype, so the walk is bounded
    // by the hierarchy; the limit only guards a malformed library's cycle.
    for _ in 0..64 {
        let function = pending.pop()?;
        if let Some(key) = signature_key(&function.base.symbol.signature) {
            return Some(key);
        }
        for symbol in &function.overridden {
            if let Some(key) = signature_key(&symbol.signature) {
                return Some(key);
            }
        }
        pending.extend(function.overridden.iter().filter_map(|symbol| {
            matches!(symbol.signature, KlibIrSignature::FileLocal { .. })
                .then(|| bodies.module_local_function(module, &symbol.signature))
                .flatten()
                .map(|(_, function)| function)
        }));
    }
    None
}
