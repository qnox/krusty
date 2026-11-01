//! Exact declaration-owned class membership and formal bindings shared by member calls.

use super::decline::KlibBodyDeclineReason;
use super::klib_types::{classifier_name, semantic_type, KlibTypeParameters};
use crate::fx_hash::HashSet;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{KlibBodyCallable, KlibBodyClassContract, KlibBodyClassParameters};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrClass, KlibIrFunction, KlibIrVariance};
use crate::metadata::klib_ir::KlibIrSymbolKind;
use crate::metadata::klib_ir::{
    tree::{KlibIrNullability, KlibIrType},
    KlibIrSignature,
};
use crate::types::{Ty, TyKind, TypeVariance};

pub(super) struct ClassMemberHeader<'a, 'b> {
    pub(super) contract: &'a KlibBodyClassContract,
    pub(super) arena: &'b KlibIrArena,
    pub(super) function: &'b KlibIrFunction,
    pub(super) class: &'b KlibIrClass,
    pub(super) types: KlibTypeParameters,
    pub(super) formals: Vec<String>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// Semantic type conversion interns class paths, so prove the raw identity, including its mask,
/// before using a member's dispatch receiver as a semantic class application.
pub(super) fn authenticate_receiver(
    class: &KlibBodyClassContract,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<(), KlibBodyDeclineReason> {
    let dispatch = function
        .dispatch_receiver
        .as_ref()
        .ok_or_else(|| mismatch("a class member has no serialized dispatch receiver"))?;
    if !matches!(arena.ty(dispatch.ty), KlibIrType::Simple { classifier,
        nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull, .. }
        if classifier.kind == KlibIrSymbolKind::Class
            && classifier.signature == KlibIrSignature::Public(class.signature.clone()))
    {
        return Err(mismatch(
            "the serialized member receiver names a different class identity",
        ));
    }
    Ok(())
}

pub(super) fn declared_receiver(class: &KlibBodyClassContract) -> Ty {
    if class.parameters.formals.is_empty() {
        crate::types::builtin_semantic(class.classifier)
            .unwrap_or_else(|| Ty::obj_name(class.classifier))
    } else {
        Ty::obj_args_name(class.classifier, &class.parameters.formals)
    }
}

pub(super) fn authenticate<'a, 'b>(
    selected: KlibBodyCallable<'a>,
    bodies: &'b KlibDeclarationBodies,
) -> Result<ClassMemberHeader<'a, 'b>, KlibBodyDeclineReason> {
    let class = selected
        .class_contract()
        .ok_or_else(|| mismatch("a selected member has no provider-owned class contract"))?;
    if class.module != selected.identity().module
        || class.signature.member_id().is_some()
        || selected.declaration_owner()
            != Some(crate::types::SemanticCallableOwner::Classifier(
                class.classifier,
            ))
        || classifier_name(&class.signature)? != class.classifier
    {
        return Err(mismatch(
            "the class identity disagrees with its selected member",
        ));
    }
    let member = bodies
        .class_member_header(selected.identity(), &class.signature)
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("a selected member has no exact serialized class header"))?;
    if member.class.base.symbol.kind != KlibIrSymbolKind::Class {
        return Err(mismatch(
            "the serialized class header has a non-class symbol",
        ));
    }
    let (types, formals) = bind_class_parameters(&class.parameters, member.arena, member.class)?;
    Ok(ClassMemberHeader {
        contract: class,
        arena: member.arena,
        function: member.function,
        class: member.class,
        types,
        formals,
    })
}

fn bind_class_parameters(
    parameters: &KlibBodyClassParameters,
    arena: &KlibIrArena,
    raw_class: &KlibIrClass,
) -> Result<(KlibTypeParameters, Vec<String>), KlibBodyDeclineReason> {
    let count = parameters.formals.len();
    if parameters.own_count != count {
        return Err(KlibBodyDeclineReason::UnsupportedDeclaration(
            "captured class type parameters",
        ));
    }
    if raw_class.type_parameters.len() != count
        || parameters.bounds.len() != count
        || parameters.variances.len() != count
    {
        return Err(mismatch(
            "the serialized class formals are not parallel to the provider contract",
        ));
    }
    let mut types = KlibTypeParameters::default();
    let mut identities = HashSet::default();
    let mut formals = Vec::with_capacity(count);
    for (index, parameter) in raw_class.type_parameters.iter().enumerate() {
        let TyKind::TyParam(identity, _) = parameters.formals[index].kind() else {
            return Err(mismatch("a selected class formal is not a type parameter"));
        };
        let variance = match parameters.variances[index] {
            TypeVariance::Invariant => KlibIrVariance::Invariant,
            TypeVariance::In => KlibIrVariance::In,
            TypeVariance::Out => KlibIrVariance::Out,
        };
        if !identities.insert(identity)
            || parameter.base.symbol.kind != KlibIrSymbolKind::TypeParameter
            || !types.bind(&parameter.base.symbol, parameters.formals[index])
            || parameter.is_reified()
            || parameter.variance() != Some(variance)
        {
            return Err(mismatch("the serialized class formal identity, variance or reification disagrees with its provider"));
        }
        formals.push(identity.as_str().to_owned());
    }
    for (index, parameter) in raw_class.type_parameters.iter().enumerate() {
        let bounds = parameter
            .supertypes
            .iter()
            .map(|ty| semantic_type(arena, &types, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        if normalized_bounds(&bounds) != normalized_bounds(&parameters.bounds[index]) {
            return Err(mismatch(
                "the serialized class upper bounds disagree with its provider",
            ));
        }
        if !types.record_bounds(parameters.formals[index], parameters.bounds[index].clone()) {
            return Err(mismatch("a class repeats a semantic formal identity"));
        }
    }
    Ok((types, formals))
}

fn normalized_bounds(bounds: &[Ty]) -> Vec<Ty> {
    if bounds.is_empty() {
        vec![Ty::nullable(Ty::obj("kotlin/Any"))]
    } else {
        bounds.to_vec()
    }
}
