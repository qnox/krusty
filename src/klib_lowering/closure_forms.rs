//! Serialized closure forms shared by emitted-body preparation and common lowering.

use crate::ir::IrCapturingCallable;
use crate::metadata::klib_ir::tree::KlibIrType;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrExpr, KlibIrExprKind, KlibIrFunctionId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};

pub(super) fn source_form(origin: Option<&str>) -> Option<IrCapturingCallable> {
    match origin {
        Some("LAMBDA") => Some(IrCapturingCallable::Lambda),
        Some("ANONYMOUS_FUNCTION") => Some(IrCapturingCallable::AnonymousFunction),
        _ => None,
    }
}

pub(super) fn unbound_rich_reference(kind: &KlibIrExprKind) -> bool {
    matches!(kind, KlibIrExprKind::RichFunctionReference {
        bound_values, reflection_target, flags, ..
    } if bound_values.is_empty() && reflection_target.is_none() && *flags == 0)
}

/// A reached closure implementation whose serialized form common lowering emits. This checks the
/// outer function class without binding its arguments: enclosing generic parameters remain valid.
/// Capture mutability and complete header/type agreement are still checked in the enclosing frame
/// by body lowering. This edge grants no intrinsic authority or admission of those later checks.
pub(crate) fn emitted_closure_function(
    arena: &KlibIrArena,
    expression: &KlibIrExpr,
) -> Option<KlibIrFunctionId> {
    if let Some(reference) = super::callable_references::declaration_reference(arena, expression) {
        return Some(reference.invoke);
    }
    if !matches!(
        expression.kind,
        KlibIrExprKind::FunctionExpression { .. } | KlibIrExprKind::RichFunctionReference { .. }
    ) {
        return None;
    }
    // A lambda converted to a functional interface has the interface's class type, and
    // implements the interface method its reference overrides rather than a function invocation.
    let shape = super::klib_types::closure_type_arity(arena, expression.ty?);
    let suspend = shape.is_some_and(|(_, suspend)| suspend);
    let (function, origin) = match &expression.kind {
        KlibIrExprKind::FunctionExpression { function, origin } if shape.is_some() => {
            (*function, origin)
        }
        KlibIrExprKind::RichFunctionReference {
            invoke,
            overridden,
            origin,
            ..
        } if unbound_rich_reference(&expression.kind) => {
            let KlibIrSignature::Public(signature) = &overridden.signature else {
                return None;
            };
            // A function-typed reference overrides its function class's invocation at the type's
            // arity. A lambda converted to an interface overrides a member of no function class.
            let implements = match shape {
                Some((arity, suspend)) => {
                    let invocation = if suspend {
                        super::klib_types::suspend_function_invocation_arity(signature)
                    } else {
                        super::klib_types::function_invocation_arity(signature)
                    };
                    invocation == Some(arity)
                }
                None => !super::klib_types::is_function_classifier_member(signature),
            };
            if overridden.kind != KlibIrSymbolKind::Function || !implements {
                return None;
            }
            (*invoke, origin)
        }
        _ => return None,
    };
    source_form(origin.as_deref())?;
    let declaration = arena.function(function);
    // A suspend closure is a suspend function exactly when its type is a suspend function type.
    (!declaration.constructor
        && (declaration.base.flags & crate::metadata::function_flags::IS_SUSPEND != 0) == suspend
        && matches!(declaration.body, Some(KlibIrBody::Block(_))))
    .then_some(function)
}

/// The functional interface an emitted closure is converted to: a lambda of the interface's class
/// type, which implements the interface method its reference overrides. Its checked SAM
/// declaration is frozen with the conversions of function values.
pub(crate) fn converted_closure<'a>(
    arena: &'a KlibIrArena,
    expression: &KlibIrExpr,
) -> Option<&'a KlibIrSymbol> {
    if !matches!(
        expression.kind,
        KlibIrExprKind::RichFunctionReference { .. }
    ) || super::callable_references::declaration_reference(arena, expression).is_some()
    {
        return None;
    }
    let ty = expression.ty?;
    if super::klib_types::closure_type_arity(arena, ty).is_some() {
        return None;
    }
    emitted_closure_function(arena, expression)?;
    match arena.ty(ty) {
        KlibIrType::Simple { classifier, .. } => Some(classifier),
        _ => None,
    }
}
