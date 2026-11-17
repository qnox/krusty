//! Serialized declaration types use the shared header interpreter; lowering retains its exact
//! body-decline boundary, including errors supplied by the owning private-class resolver.

use super::decline::KlibBodyDeclineReason;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::semantic_types;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrTypeId};
use crate::metadata::klib_ir::KlibIrSignature;
use crate::types::{Ty, TypeName};

pub(super) use semantic_types::{
    annotation_classifier, closure_type_arity, function_invocation_arity,
    is_function_classifier_member, reference_invocation_arity, suspend_function_invocation_arity,
    KlibTypeParameters,
};

pub(super) fn semantic_type(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
) -> Result<Ty, KlibBodyDeclineReason> {
    semantic_types::semantic_type(arena, parameters, id).map_err(Into::into)
}

pub(super) fn semantic_type_in(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<Ty, KlibBodyDeclineReason> {
    semantic_types::semantic_type_in(arena, parameters, id, local_class)
}

pub(super) fn classifier_name(
    signature: &KlibPublicIdSignature,
) -> Result<TypeName, KlibBodyDeclineReason> {
    semantic_types::classifier_name(signature).map_err(Into::into)
}

pub(super) fn classifier_declaration(
    signature: &KlibPublicIdSignature,
) -> Result<crate::types::ClassifierDeclarationName, KlibBodyDeclineReason> {
    semantic_types::classifier_declaration(signature).map_err(Into::into)
}

pub(super) fn member_owner(
    member: &KlibPublicIdSignature,
) -> Result<TypeName, KlibBodyDeclineReason> {
    semantic_types::member_owner(member).map_err(Into::into)
}

/// The package name-tree node whose segments are `package`; [`TypeName::ROOT`] for none.
pub(super) fn package_name(package: &[String]) -> Result<TypeName, KlibBodyDeclineReason> {
    semantic_types::package_name(package).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::tree::{KlibIrNullability, KlibIrType};
    use crate::metadata::klib_ir::{KlibIrSymbol, KlibIrSymbolKind};

    #[test]
    fn a_private_classifier_decline_keeps_the_owning_phase_error() {
        let mut arena = KlibIrArena::default();
        let classifier = KlibIrSymbol {
            kind: KlibIrSymbolKind::Class,
            signature: KlibIrSignature::FileLocal { file: 4, slot: 7 },
        };
        let ty = arena.push_type(KlibIrType::Simple {
            classifier,
            nullability: KlibIrNullability::NotSpecified,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        let refusal = KlibBodyDeclineReason::UnsupportedOperation(
            "a classifier whose owner has not been admitted",
        );
        assert_eq!(
            semantic_type_in(&arena, &KlibTypeParameters::default(), ty, &|_| Err(
                refusal.clone()
            )),
            Err(refusal)
        );
    }

    #[test]
    fn a_shared_type_error_preserves_the_body_boundary_category_and_message() {
        let mut arena = KlibIrArena::default();
        let ty = arena.push_type(KlibIrType::Dynamic {
            annotations: Vec::new(),
        });
        assert_eq!(
            semantic_type(&arena, &KlibTypeParameters::default(), ty),
            Err(KlibBodyDeclineReason::UnsupportedType("the dynamic type"))
        );
    }
}
