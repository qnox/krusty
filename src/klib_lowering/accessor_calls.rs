//! Exact property identity and declared ABI shared by dependency getter preparation and linking.

use super::body_lowering::{LinkedAccessor, LinkedAccessorTarget};
use super::decline::KlibBodyDeclineReason;
use super::function_lowering::{
    raw_function_header, raw_function_header_with_class, HeaderPurpose,
};
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
    if !matches!(
        selected.kind(),
        crate::libraries::ExternalCallableKind::Member
            | crate::libraries::ExternalCallableKind::Extension
            | crate::libraries::ExternalCallableKind::TopLevel
    ) {
        return Ok(None);
    }
    let identity = selected.identity();
    let (arena, function) = bodies.function(identity).ok_or_else(|| {
        KlibBodyDeclineReason::SignatureMismatch(
            "the selected accessor has no serialized function header".to_owned(),
        )
    })?;
    let class = if selected.is_member() {
        let class = super::class_headers::authenticate(selected, bodies)?;
        super::class_headers::authenticate_receiver(class.contract, class.arena, class.function)?;
        Some(class)
    } else {
        None
    };
    let header = if let Some(class) = class.as_ref() {
        raw_function_header_with_class(
            Some(selected),
            HeaderPurpose::External,
            class.arena,
            class.function,
            class.types.clone(),
        )?
    } else {
        raw_function_header(Some(selected), HeaderPurpose::External, arena, function)?
    };
    let (params, ret, context_count, extension, receiver) = header.linked_signature();
    let selected_extension = selected
        .generic_signature()
        .is_some_and(|signature| signature.receiver.is_some());
    if extension != selected_extension {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "the accessor receiver role differs from its selected declaration".to_owned(),
        ));
    }
    let (declared_receiver, class_type_parameters) = match class {
        Some(class) => {
            let declared = super::class_headers::declared_receiver(class.contract);
            if receiver != Some(declared) {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "the accessor receiver differs from its declaring class application".to_owned(),
                ));
            }
            (Some(declared), class.formals)
        }
        None => {
            if receiver.is_some() {
                return Err(KlibBodyDeclineReason::SignatureMismatch(
                    "a top-level accessor has a dispatch receiver".to_owned(),
                ));
            }
            (None, Vec::new())
        }
    };
    let generic = selected.generic_signature();
    let type_parameters = selected
        .type_parameters()
        .iter()
        .map(|parameter| match parameter.kind() {
            crate::types::TyKind::TyParam(name, _) => Some(name.clone()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "an accessor formal is not a type parameter".to_owned(),
            )
        })?;
    let type_parameter_bounds = generic
        .map(|signature| signature.formal_bounds.clone())
        .unwrap_or_default();
    let reified_type_parameters = selected.reified_type_parameter_ordinals().to_vec();
    if type_parameter_bounds.len() != type_parameters.len() {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "an accessor's generic bounds do not match its formal list".to_owned(),
        ));
    }
    Ok(Some(LinkedAccessor {
        target: LinkedAccessorTarget::External(accessor.property),
        write: false,
        receiver: declared_receiver,
        params: params.to_vec(),
        ret,
        context_count,
        class_type_parameters,
        callable: None,
        type_parameters,
        type_parameter_bounds,
        reified_type_parameters,
    }))
}
