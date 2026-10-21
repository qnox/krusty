//! Declaration-owned formals of ordinary (not prepared-inline) serialized function bodies.

use super::super::klib_types::{semantic_type, KlibTypeParameters};
use super::{mismatch, KlibBodyCallable, KlibBodyDeclineReason, KlibIrArena, KlibIrFunction};
use crate::fx_hash::HashSet;
use crate::metadata::klib_ir::tree::KlibIrVariance;
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::types::{Ty, TyKind};

pub(super) fn bind(
    selected: Option<KlibBodyCallable<'_>>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<KlibTypeParameters, KlibBodyDeclineReason> {
    let Some(selected) = selected else {
        // A header-only generic declaration needs its producer-qualified provider formals before
        // it can be emitted. Never manufacture semantic identities from serialized source names.
        return if function.type_parameters.is_empty() {
            Ok(KlibTypeParameters::default())
        } else {
            Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "type parameters",
            ))
        };
    };
    let generic = selected.generic_signature();
    let count = generic.map_or(0, |signature| signature.formals.len());
    if function.type_parameters.len() != count {
        return Err(mismatch(format!(
            "{} serialized type parameters, {count} selected",
            function.type_parameters.len(),
        )));
    }
    if selected.type_parameters().len() != count {
        return Err(mismatch(
            "the selected formal types are not parallel to its generic signature".to_owned(),
        ));
    }
    if let Some(signature) = generic {
        if signature.formal_bounds.len() != count {
            return Err(mismatch(
                "the selected generic bounds are not parallel to its formals".to_owned(),
            ));
        }
        let receiver = selected.parameter_identities().iter().position(|identity| {
            matches!(
                identity,
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver
            )
        });
        if signature.receiver.is_some() != receiver.is_some()
            || receiver.is_some_and(|index| index != selected.context_count())
        {
            return Err(mismatch(
                "the selected generic receiver disagrees with its callable parameter roles"
                    .to_owned(),
            ));
        }
        if signature
            .parameters_with_receiver(selected.context_count())
            .as_ref()
            != selected.params()
            || signature.ret != selected.ret()
        {
            return Err(mismatch(
                "the selected generic signature disagrees with its callable parameters or result"
                    .to_owned(),
            ));
        }
    }
    let ordinals = selected.reified_type_parameter_ordinals();
    let mut reified = HashSet::default();
    for &ordinal in ordinals {
        if ordinal as usize >= count || !reified.insert(ordinal as usize) {
            return Err(mismatch(
                "the selected reified type-parameter ordinals are invalid".to_owned(),
            ));
        }
    }
    let mut types = KlibTypeParameters::default();
    let mut identities = HashSet::default();
    for (index, parameter) in function.type_parameters.iter().enumerate() {
        let signature = generic.expect("a nonempty formal list has a selected generic signature");
        let name = &signature.formals[index];
        let formal = selected.type_parameters()[index];
        if !matches!(formal.kind(), TyKind::TyParam(identity, _) if identity.as_str() == name) {
            return Err(mismatch(format!(
                "type parameter {index} has a different selected formal identity"
            )));
        }
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter
            || !identities.insert(name)
            || !types.bind(&parameter.base.symbol, formal)
        {
            return Err(mismatch(format!(
                "type parameter {index} has no distinct serialized and selected identity"
            )));
        }
        if parameter.variance() != Some(KlibIrVariance::Invariant) {
            return Err(mismatch(format!(
                "function type parameter {index} is not invariant"
            )));
        }
        if parameter.is_reified() != reified.contains(&index) {
            return Err(mismatch(format!(
                "type parameter {index} has different serialized and selected reification"
            )));
        }
    }
    // Bind all symbols before reading bounds: forward references and F-bounds are legal, and
    // the provider already owns their finite semantic representation.
    for (index, parameter) in function.type_parameters.iter().enumerate() {
        let signature = generic.expect("a nonempty formal list has a selected generic signature");
        let serialized = parameter
            .supertypes
            .iter()
            .map(|bound| semantic_type(arena, &types, *bound))
            .collect::<Result<Vec<_>, _>>()?;
        if normalized_bounds(&serialized) != normalized_bounds(&signature.formal_bounds[index]) {
            return Err(mismatch(format!(
                "type parameter {index} has different serialized and selected upper bounds"
            )));
        }
    }
    if !reified.is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "reified type parameters",
        ));
    }
    Ok(types)
}

fn normalized_bounds(bounds: &[Ty]) -> Vec<Ty> {
    if bounds.is_empty() {
        vec![Ty::nullable(Ty::obj("kotlin/Any"))]
    } else {
        bounds.to_vec()
    }
}
