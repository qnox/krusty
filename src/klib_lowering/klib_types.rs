//! KLIB IR types as krusty's semantic types.
//!
//! A KLIB names a classifier by its exact public signature. That structural package/declaration
//! path is interned directly as the classifier's Kotlin identity, without rendering a qualified
//! spelling and parsing it back: a body's types and its selected declaration's types are one
//! model. Only non-generic classifier types are
//! modelled; type parameters, type arguments (function types among them), annotated, definitely
//! non-null, dynamic and error types decline by form.

use super::decline::KlibBodyDeclineReason;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrNullability, KlibIrType, KlibIrTypeId};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{builtin_semantic, type_name_child, Ty, TypeName};

/// The semantic type of the KLIB type `id`.
pub(super) fn semantic_type(
    arena: &KlibIrArena,
    id: KlibIrTypeId,
) -> Result<Ty, KlibBodyDeclineReason> {
    let (classifier, nullability, arguments, annotations) = match arena.ty(id) {
        KlibIrType::Simple {
            classifier,
            nullability,
            arguments,
            annotations,
            // The alias a type was written as; the type itself is its expansion.
            abbreviation: _,
        } => (classifier, nullability, arguments, annotations),
        KlibIrType::DefinitelyNotNull(_) => {
            return Err(KlibBodyDeclineReason::UnsupportedType(
                "a definitely non-null type",
            ))
        }
        KlibIrType::Dynamic { .. } => {
            return Err(KlibBodyDeclineReason::UnsupportedType("the dynamic type"))
        }
        KlibIrType::Error { .. } => {
            return Err(KlibBodyDeclineReason::UnsupportedType("an error type"))
        }
    };
    match classifier.kind {
        KlibIrSymbolKind::Class => {}
        KlibIrSymbolKind::TypeParameter => {
            return Err(KlibBodyDeclineReason::UnsupportedType("a type parameter"))
        }
        _ => {
            return Err(KlibBodyDeclineReason::UnsupportedType(
                "a type whose classifier is not a class",
            ))
        }
    }
    let KlibIrSignature::Public(signature) = &classifier.signature else {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a type of a non-public class",
        ));
    };
    if !arguments.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a type with type arguments",
        ));
    }
    if !annotations.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedType("an annotated type"));
    }
    if signature.member_id().is_some() || signature.declaration().is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a type whose classifier is not a class",
        ));
    }
    let name = classifier_name(signature)?;
    let semantic = builtin_semantic(name).unwrap_or_else(|| Ty::obj_name(name));
    Ok(
        if matches!(nullability, KlibIrNullability::MarkedNullable) {
            Ty::nullable(semantic)
        } else {
            semantic
        },
    )
}

/// Intern a classifier identity one signature segment at a time. Package segments are namespace
/// children; declaration segments after the first are nested classifiers. No qualified spelling is
/// rendered and parsed back into the name tree.
fn classifier_name(signature: &KlibPublicIdSignature) -> Result<TypeName, KlibBodyDeclineReason> {
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !signature.package().segments().iter().all(valid)
        || !signature.declaration().segments().iter().all(valid)
    {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a class with an invalid identity path",
        ));
    }
    let mut name = signature
        .package()
        .segments()
        .iter()
        .fold(TypeName::ROOT, |parent, segment| {
            type_name_child(parent, segment)
        });
    let mut declarations = signature.declaration().segments().iter();
    let first = declarations
        .next()
        .ok_or(KlibBodyDeclineReason::UnsupportedType(
            "a type whose classifier is not a class",
        ))?;
    name = type_name_child(name, first);
    for nested in declarations {
        name = name.nested_child(nested);
    }
    Ok(name)
}
