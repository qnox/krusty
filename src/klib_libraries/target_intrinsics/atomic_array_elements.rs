//! Native's `kotlin.concurrent` atomic accesses to one array element.

use crate::libraries::AtomicOperation;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrFunction, KlibIrNullability, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::Ty;

use super::{ir_semantic_type, ir_string_annotation, ir_unit, TargetIntrinsicPlatform};

/// The arrays whose elements the target accesses atomically: the specialized arrays of a signed
/// integral element, by their exact classifier identity.
const INTEGRAL_ELEMENTS: [Ty; 4] = [Ty::Byte, Ty::Short, Ty::Int, Ty::Long];

/// An external `TypedIntrinsic` extension of an array whose first parameter is the `Int` index
/// and whose remaining operands are the array's element. The receiver is a specialized integral
/// array, or `Array<T>` over the declaration's one type parameter; only an integral element adds.
pub(super) fn authenticate(
    platform: Option<TargetIntrinsicPlatform>,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Option<AtomicOperation> {
    let KlibIrSignature::Public(signature) = &function.base.symbol.signature else {
        return None;
    };
    if platform != Some(TargetIntrinsicPlatform::Native)
        || signature.package().segments() != ["kotlin", "concurrent"]
        || signature.declaration().segments().len() != 1
        || function.base.symbol.kind != KlibIrSymbolKind::Function
        || function.constructor
        || function.body.is_some()
        || function.type_parameters.len() > 1
        || function.dispatch_receiver.is_some()
        || !function.context_parameters.is_empty()
    {
        return None;
    }
    let operation = match ir_string_annotation(
        arena,
        &function.base.annotations,
        &["kotlin", "native", "internal"],
        &["TypedIntrinsic", "<init>"],
    )? {
        "ATOMIC_GET_ARRAY_ELEMENT" => AtomicOperation::Load,
        "ATOMIC_SET_ARRAY_ELEMENT" => AtomicOperation::Store,
        "GET_AND_SET_ARRAY_ELEMENT" => AtomicOperation::Exchange,
        "COMPARE_AND_SET_ARRAY_ELEMENT" => AtomicOperation::CompareAndSet,
        "COMPARE_AND_EXCHANGE_ARRAY_ELEMENT" => AtomicOperation::CompareAndExchange,
        "GET_AND_ADD_ARRAY_ELEMENT" => AtomicOperation::FetchAndAdd,
        _ => return None,
    };
    let element = element(arena, function)?;
    let is_element = |id| match element {
        Element::Integral(scalar) => ir_semantic_type(arena, id) == Some(scalar),
        Element::TypeParameter(value) => arena.ty(id) == arena.ty(value),
    };
    let [index, operands @ ..] = function.regular_parameters.as_slice() else {
        return None;
    };
    if ir_semantic_type(arena, index.ty) != Some(Ty::Int)
        || operands.len() != operation.operands()
        || operands.iter().any(|operand| !is_element(operand.ty))
        || (operation == AtomicOperation::FetchAndAdd
            && matches!(element, Element::TypeParameter(_)))
    {
        return None;
    }
    let answers = match operation {
        AtomicOperation::Store => ir_unit(arena, function.return_type),
        AtomicOperation::CompareAndSet => {
            ir_semantic_type(arena, function.return_type) == Some(Ty::Boolean)
        }
        AtomicOperation::Load
        | AtomicOperation::Exchange
        | AtomicOperation::CompareAndExchange
        | AtomicOperation::FetchAndAdd => is_element(function.return_type),
    };
    answers.then_some(operation)
}

#[derive(Clone, Copy)]
enum Element {
    Integral(Ty),
    /// The declaration's type parameter, as the receiver `Array<T>` applies it.
    TypeParameter(KlibIrTypeId),
}

/// The element of the declaration's receiver array, which the serializer records as definitely
/// not null.
fn element(arena: &KlibIrArena, function: &KlibIrFunction) -> Option<Element> {
    let KlibIrType::Simple {
        classifier,
        nullability: KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull,
        arguments,
        annotations,
        abbreviation: None,
    } = arena.ty(function.extension_receiver.as_ref()?.ty)
    else {
        return None;
    };
    let KlibIrSignature::Public(array) = &classifier.signature else {
        return None;
    };
    if classifier.kind != KlibIrSymbolKind::Class || !annotations.is_empty() {
        return None;
    }
    match (function.type_parameters.as_slice(), arguments.as_slice()) {
        ([], []) => INTEGRAL_ELEMENTS
            .into_iter()
            .find(|&element| {
                Ty::array(element)
                    .obj_internal()
                    .is_some_and(|class| *array == KlibPublicIdSignature::classifier(class))
            })
            .map(Element::Integral),
        ([parameter], [KlibIrTypeArgument::Type { ty, .. }])
            if *array == KlibPublicIdSignature::classifier(crate::types::wk::array()) =>
        {
            matches!(
                arena.ty(*ty),
                KlibIrType::Simple {
                    classifier,
                    nullability: KlibIrNullability::NotSpecified,
                    arguments,
                    annotations,
                    abbreviation: None,
                } if classifier == &parameter.base.symbol
                    && arguments.is_empty()
                    && annotations.is_empty()
            )
            .then_some(Element::TypeParameter(*ty))
        }
        _ => None,
    }
}
