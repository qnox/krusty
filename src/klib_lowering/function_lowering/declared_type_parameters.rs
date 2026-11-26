//! Declaration-owned formals of ordinary (not prepared-inline) serialized function bodies.

use super::super::klib_types::{semantic_type, semantic_type_in, KlibTypeParameters};
use super::{mismatch, KlibBodyCallable, KlibBodyDeclineReason, KlibIrArena, KlibIrFunction};
use crate::fx_hash::HashSet;
use crate::metadata::klib_ir::tree::KlibIrVariance;
use crate::metadata::klib_ir::KlibIrSignature;
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::types::{Ty, TyKind, TypeName};

pub(super) fn bind(
    selected: Option<KlibBodyCallable<'_>>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    mut types: KlibTypeParameters,
) -> Result<KlibTypeParameters, KlibBodyDeclineReason> {
    let Some(selected) = selected else {
        // A header-only generic declaration needs its producer-qualified provider formals before
        // it can be emitted, or the declaration-owned formals a file-private declaration's own
        // identity mints (`file_private_formals`). Never manufacture semantic identities from
        // serialized source names for it. A private member has no provider record: its formals
        // are its own.
        return if function
            .type_parameters
            .iter()
            .all(|parameter| types.binds(&parameter.base.symbol))
        {
            Ok(types)
        } else if crate::metadata::semantic::declaration_visibility(function.base.flags)
            == crate::types::Visibility::Private
        {
            own(function, arena, types, &|_| Ok(None))
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
        if !types.record_bounds(
            selected.type_parameters()[index],
            signature.formal_bounds[index].clone(),
        ) {
            return Err(mismatch(
                "a function formal repeats an enclosing formal identity".to_owned(),
            ));
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

/// `types` with the own type parameters of `function`, a declaration no provider records (a
/// private declaration, or a member of a non-public class), bound to fresh formals with their
/// declared bounds. `local_class` admits the non-public classifiers its bounds may name.
pub(in super::super) fn own(
    function: &KlibIrFunction,
    arena: &KlibIrArena,
    mut types: KlibTypeParameters,
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<KlibTypeParameters, KlibBodyDeclineReason> {
    let mut formals = Vec::with_capacity(function.type_parameters.len());
    for parameter in &function.type_parameters {
        if parameter.is_reified() {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a reified type parameter of an unrecorded declaration",
            ));
        }
        if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter {
            return Err(mismatch(
                "an unrecorded declaration's type parameter has no symbol".to_owned(),
            ));
        }
        let formal = Ty::ty_param(
            crate::types::metadata_type_parameter(&parameter.name),
            Ty::nullable(Ty::obj("kotlin/Any")),
        );
        if !types.bind(&parameter.base.symbol, formal) {
            return Err(mismatch(
                "an unrecorded declaration repeats a type parameter".to_owned(),
            ));
        }
        formals.push(formal);
    }
    // Bounds may name any formal, so all are bound before any is read.
    for (parameter, formal) in function.type_parameters.iter().zip(formals) {
        let bounds = parameter
            .supertypes
            .iter()
            .map(|bound| semantic_type_in(arena, &types, *bound, local_class))
            .collect::<Result<Vec<_>, _>>()?;
        if !types.record_bounds(formal, bounds) {
            return Err(mismatch(
                "an unrecorded declaration's type parameter repeats an enclosing formal".to_owned(),
            ));
        }
    }
    Ok(types)
}

/// The formal identity minted for each (file-private declaration, ordinal).
type MintedFormals =
    crate::fx_hash::HashMap<(crate::libraries::KlibDependencyCallable, usize), &'static str>;

static FILE_PRIVATE_FORMALS: std::sync::OnceLock<std::sync::Mutex<MintedFormals>> =
    std::sync::OnceLock::new();

/// A file-private declaration's bound formals, their identities and each one's complete bounds.
pub(in super::super) type FilePrivateFormals = (
    KlibTypeParameters,
    Vec<crate::types::TyParamName>,
    Vec<Vec<Ty>>,
);

/// The formals of a file-private function no provider records: one semantic identity per
/// (declaration identity, ordinal), minted once as a provider mints a published declaration's per
/// (signature, ordinal), each bound to its serialized symbol with the bounds its serialized
/// supertypes state. The identities are opaque; the serialized name is kept only as their
/// spelling. In declaration order, with each formal's complete bounds.
pub(in super::super) fn file_private_formals(
    identity: &crate::libraries::KlibDependencyCallable,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<FilePrivateFormals, KlibBodyDeclineReason> {
    let mut types = KlibTypeParameters::default();
    let mut formals = Vec::with_capacity(function.type_parameters.len());
    {
        let mut minted = FILE_PRIVATE_FORMALS
            .get_or_init(|| std::sync::Mutex::new(crate::fx_hash::HashMap::default()))
            .lock()
            .unwrap();
        for (ordinal, parameter) in function.type_parameters.iter().enumerate() {
            if parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter {
                return Err(mismatch(format!(
                    "type parameter {ordinal} has no serialized type-parameter symbol"
                )));
            }
            if parameter.variance() != Some(KlibIrVariance::Invariant) {
                return Err(mismatch(format!(
                    "function type parameter {ordinal} is not invariant"
                )));
            }
            if parameter.is_reified() {
                return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                    "reified type parameters",
                ));
            }
            let name = *minted
                .entry((identity.clone(), ordinal))
                .or_insert_with(|| crate::types::metadata_type_parameter(&parameter.name));
            let formal = Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any")));
            if !types.bind(&parameter.base.symbol, formal) {
                return Err(mismatch(format!(
                    "type parameter {ordinal} repeats a serialized symbol"
                )));
            }
            formals.push(name);
        }
    }
    // Bind all symbols before reading bounds: forward references and F-bounds are legal.
    let mut bounds = Vec::with_capacity(formals.len());
    for (parameter, &name) in function.type_parameters.iter().zip(&formals) {
        let declared = parameter
            .supertypes
            .iter()
            .map(|bound| semantic_type(arena, &types, *bound))
            .collect::<Result<Vec<_>, _>>()?;
        let declared = normalized_bounds(&declared);
        // The formal carries its erasure as its bound; only the implicit `Any?` is known before
        // the bounds that may name the formals themselves are read.
        if declared != normalized_bounds(&[]) {
            return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
                "a bounded type parameter of a file-private declaration",
            ));
        }
        if !types.record_bounds(
            Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any"))),
            declared.clone(),
        ) {
            return Err(mismatch(
                "a file-private formal repeats an enclosing formal identity".to_owned(),
            ));
        }
        bounds.push(declared);
    }
    Ok((
        types,
        formals
            .into_iter()
            .map(crate::types::TyParamName::new)
            .collect(),
        bounds,
    ))
}
