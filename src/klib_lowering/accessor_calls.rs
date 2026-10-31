//! Exact property identity and declared ABI shared by dependency getter preparation and linking.

use super::body_lowering::LinkedAccessor;
use super::decline::KlibBodyDeclineReason;
use super::function_lowering::{raw_function_header_with_class, HeaderPurpose};
use crate::libraries::{KlibAccessorRole, KlibBodyCallable};

pub(super) fn signature(
    selected: KlibBodyCallable<'_>,
) -> Option<crate::libraries::DependencyGetterSignature<'_>> {
    let accessor = selected.accessor()?;
    if accessor.role != KlibAccessorRole::Getter
        || selected.kind() != crate::libraries::ExternalCallableKind::Member
    {
        return None;
    }
    Some(crate::libraries::DependencyGetterSignature {
        owner: selected.class_contract()?.classifier,
        name: &accessor.name,
        parameters: selected.params(),
        result: selected.ret(),
    })
}

pub(super) fn authenticate(
    selected: KlibBodyCallable<'_>,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
) -> Result<Option<LinkedAccessor>, KlibBodyDeclineReason> {
    let Some(accessor) = selected.accessor() else {
        return Ok(None);
    };
    if !matches!(
        selected.signature(),
        crate::libraries::KlibDeclarationSignature::Accessor(_)
    ) {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "a getter contract belongs to a non-accessor declaration".to_owned(),
        ));
    }
    if accessor.role != KlibAccessorRole::Getter {
        return Ok(None);
    }
    if selected.kind() != crate::libraries::ExternalCallableKind::Member {
        return Ok(None);
    }
    let class = super::class_headers::authenticate(selected, bodies)?;
    super::class_headers::authenticate_receiver(class.contract, class.arena, class.function)?;
    let declared_receiver = super::class_headers::declared_receiver(class.contract);
    let header = raw_function_header_with_class(
        Some(selected),
        HeaderPurpose::External,
        class.arena,
        class.function,
        class.types,
    )?;
    let (params, ret, context_count, extension, receiver) = header.linked_signature();
    if extension || !selected.type_parameters().is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "an extension or generic dependency getter",
        ));
    }
    if receiver != Some(declared_receiver) {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "the getter receiver differs from its declaring class application".to_owned(),
        ));
    }
    Ok(Some(LinkedAccessor {
        property: accessor.property,
        write: false,
        receiver: Some(declared_receiver),
        params: params.to_vec(),
        ret,
        context_count,
        class_type_parameters: class.formals,
    }))
}
