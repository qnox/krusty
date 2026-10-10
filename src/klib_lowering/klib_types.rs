//! KLIB IR types as krusty's semantic types.
//!
//! A KLIB names a classifier by its exact public signature. That path is the classifier's Kotlin
//! class id, so the type is normalized through the same target-free metadata conversion
//! ([`semantic_ty`]) the KLIB provider applies to the declaration's metadata: a body's types and
//! its selected declaration's types are one model. Only non-generic classifier types are
//! modelled; type parameters, type arguments (function types among them), annotated, definitely
//! non-null, dynamic and error types decline by form.

use std::collections::HashMap;

use super::decline::KlibBodyDeclineReason;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrNullability, KlibIrType, KlibIrTypeId};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::metadata::semantic::{semantic_ty, KotlinFunctionTypeShape, KotlinType};
use crate::types::Ty;

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
    // Metadata's class id: the package path, then the class path within it.
    let package = signature.package().segments().join("/");
    let class = signature.declaration().segments().join(".");
    let internal = if package.is_empty() {
        class
    } else {
        format!("{package}/{class}")
    };
    let metadata = KotlinType::Class {
        internal,
        args: Vec::new(),
        nullable: matches!(nullability, KlibIrNullability::MarkedNullable),
        shape: KotlinFunctionTypeShape::default(),
    };
    Ok(semantic_ty(&metadata, &HashMap::new()))
}
