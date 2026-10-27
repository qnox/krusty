//! Serialized closure forms shared by emitted-body preparation and common lowering.

use crate::ir::IrCapturingCallable;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrExpr, KlibIrExprKind, KlibIrFunctionId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};

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
    if let Some(reference) = super::callable_references::unbound_reference(arena, expression) {
        return Some(reference.invoke);
    }
    if !matches!(
        expression.kind,
        KlibIrExprKind::FunctionExpression { .. } | KlibIrExprKind::RichFunctionReference { .. }
    ) {
        return None;
    }
    let arity = super::klib_types::closure_type_arity(arena, expression.ty?)?;
    let (function, origin) = match &expression.kind {
        KlibIrExprKind::FunctionExpression { function, origin } => (*function, origin),
        KlibIrExprKind::RichFunctionReference {
            invoke,
            overridden,
            origin,
            ..
        } if unbound_rich_reference(&expression.kind) => {
            let KlibIrSignature::Public(signature) = &overridden.signature else {
                return None;
            };
            if overridden.kind != KlibIrSymbolKind::Function
                || super::klib_types::function_invocation_arity(signature) != Some(arity)
            {
                return None;
            }
            (*invoke, origin)
        }
        _ => return None,
    };
    source_form(origin.as_deref())?;
    let declaration = arena.function(function);
    (!declaration.constructor
        && declaration.base.flags & crate::metadata::function_flags::IS_SUSPEND == 0
        && matches!(declaration.body, Some(KlibIrBody::Block(_))))
    .then_some(function)
}
