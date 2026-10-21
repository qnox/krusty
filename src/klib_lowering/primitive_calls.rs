//! Positive common-IR capability of provider-normalized primitive declarations.
//!
//! Both dependency closure planning and body lowering consume this authenticated call shape.
//! A known operation with a malformed header is an error, never permission to walk its private
//! implementation or to call a target helper instead.

use super::decline::KlibBodyDeclineReason;
use super::function_lowering::{raw_function_header_with_class, FunctionHeader, HeaderPurpose};
use super::klib_types::{classifier_name, semantic_type, KlibTypeParameters};
use crate::fx_hash::HashSet;
use crate::ir::IrIntrinsic;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{
    CompilerIntrinsic, KlibBodyCallable, KlibBodyClassParameters, KlibDeclarationSignature,
    PrimitiveUnaryIntrinsic,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrClass, KlibIrNullability, KlibIrType, KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{Ty, TyKind, TypeVariance};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommonPrimitive {
    Unary(PrimitiveUnaryIntrinsic),
    Intrinsic(IrIntrinsic),
}

pub(crate) struct CommonPrimitiveCall {
    pub(super) operation: CommonPrimitive,
    pub(super) receiver: Ty,
    pub(super) ret: Ty,
    pub(super) class_type_parameters: Vec<String>,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

pub(super) fn authenticate(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Option<CommonPrimitiveCall>, KlibBodyDeclineReason> {
    let operation = match selected.compiler_intrinsic() {
        Some(CompilerIntrinsic::PrimitiveUnary(operation)) => CommonPrimitive::Unary(operation),
        Some(CompilerIntrinsic::ArraySize) => CommonPrimitive::Intrinsic(IrIntrinsic::ArraySize),
        Some(CompilerIntrinsic::StringLength) => {
            CommonPrimitive::Intrinsic(IrIntrinsic::StringLength)
        }
        // Library algorithms and target instructions are deliberately not common primitives.
        _ => return Ok(None),
    };
    if !selected.is_member()
        || !selected.params().is_empty()
        || selected.context_count() != 0
        || !selected.type_parameters().is_empty()
    {
        return Err(mismatch(
            "a common primitive has a different selected call shape",
        ));
    }
    let signature_kind_matches = match operation {
        CommonPrimitive::Unary(_) => {
            matches!(selected.signature(), KlibDeclarationSignature::Public(_))
        }
        CommonPrimitive::Intrinsic(_) => {
            matches!(selected.signature(), KlibDeclarationSignature::Accessor(_))
        }
    };
    if !signature_kind_matches {
        return Err(mismatch(
            "a common primitive has a different declaration kind",
        ));
    }
    let (header, formals) = authenticated_header(selected, bodies)?;
    let (_, ret, _, _, receiver) = header.linked_signature();
    let receiver = receiver.expect("the authenticated member has dispatch");
    let valid = match operation {
        CommonPrimitive::Unary(_) => match receiver {
            Ty::Byte | Ty::Short | Ty::Int => ret == Ty::Int,
            Ty::Long => ret == Ty::Long,
            Ty::Float => ret == Ty::Float,
            Ty::Double => ret == Ty::Double,
            _ => false,
        },
        CommonPrimitive::Intrinsic(IrIntrinsic::ArraySize) => receiver.is_array() && ret == Ty::Int,
        CommonPrimitive::Intrinsic(IrIntrinsic::StringLength) => {
            receiver == Ty::String && ret == Ty::Int
        }
        _ => false,
    };
    if !valid {
        return Err(mismatch(
            "a common primitive has incompatible declared operand and result types",
        ));
    }
    Ok(Some(CommonPrimitiveCall {
        operation,
        receiver,
        ret,
        class_type_parameters: formals,
    }))
}

fn authenticated_header(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<(FunctionHeader, Vec<String>), KlibBodyDeclineReason> {
    let class = selected
        .class_contract()
        .ok_or_else(|| mismatch("a common primitive has no provider-owned class contract"))?;
    if class.module != selected.identity().module
        || selected.declaration_owner()
            != Some(crate::types::SemanticCallableOwner::Classifier(
                class.classifier,
            ))
        || classifier_name(&class.signature)? != class.classifier
    {
        return Err(mismatch(
            "the common primitive's class identity disagrees with its selected declaration",
        ));
    }
    let member = bodies
        .primitive_member_header(selected.identity(), &class.signature)
        .map_err(mismatch)?
        .ok_or_else(|| mismatch("a common primitive has no exact serialized class header"))?;
    let (arena, raw_class, function) = (member.arena, member.class, member.function);
    if raw_class.base.symbol.kind != KlibIrSymbolKind::Class {
        return Err(mismatch(
            "the serialized class header has a non-class symbol",
        ));
    }
    let parameters = &class.parameters;
    let (types, formals) = bind_class_parameters(parameters, arena, raw_class)?;
    let dispatch = function
        .dispatch_receiver
        .as_ref()
        .ok_or_else(|| mismatch("a common primitive has no serialized dispatch receiver"))?;
    // Type conversion interns class paths, not signature masks. Authenticate the complete raw
    // receiver before converting it through the class's exact formal binder.
    if !matches!(arena.ty(dispatch.ty), KlibIrType::Simple { classifier, nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull, .. }
        if classifier.kind == KlibIrSymbolKind::Class && classifier.signature == KlibIrSignature::Public(class.signature.clone()))
    {
        return Err(mismatch(
            "the serialized primitive receiver names a different class identity",
        ));
    }
    let header = raw_function_header_with_class(
        Some(selected),
        HeaderPurpose::External,
        arena,
        function,
        types,
    )?;
    let (params, _, contexts, extension, receiver) = header.linked_signature();
    let expected_receiver = if parameters.formals.is_empty() {
        crate::types::builtin_semantic(class.classifier)
            .unwrap_or_else(|| Ty::obj_name(class.classifier))
    } else {
        Ty::obj_args_name(class.classifier, &parameters.formals)
    };
    if receiver != Some(expected_receiver) || !params.is_empty() || contexts != 0 || extension {
        return Err(mismatch(
            "the serialized primitive receiver or arguments disagree with its class contract",
        ));
    }
    Ok((header, formals))
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
