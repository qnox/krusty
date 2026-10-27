//! Exact serialized declaration references and their unadapted invocation wrappers.

use crate::libraries::function_classifiers;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrExpr, KlibIrExprId, KlibIrExprKind,
    KlibIrFunctionId, KlibIrNullability, KlibIrStatement, KlibIrType,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::types::{Ty, TyKind};

pub(super) struct UnboundReference<'a> {
    pub(super) invoke: KlibIrFunctionId,
    pub(super) target: &'a KlibPublicIdSignature,
    pub(super) returned: KlibIrExprId,
    pub(super) arity: usize,
}

/// The same admission owns preparation and lowering. A reflection target alone does not prove
/// that the wrapper invokes it without receiver binding, argument adaptation or extra effects.
pub(super) fn unbound_reference<'a>(
    arena: &'a KlibIrArena,
    expression: &'a KlibIrExpr,
) -> Option<UnboundReference<'a>> {
    let KlibIrExprKind::RichFunctionReference {
        bound_values,
        reflection_target: Some(target),
        overridden,
        invoke,
        flags: 0,
        ..
    } = &expression.kind
    else {
        return None;
    };
    if !bound_values.is_empty() || target.kind != KlibIrSymbolKind::Function {
        return None;
    }
    let KlibIrSignature::Public(target_signature) = &target.signature else {
        return None;
    };
    if target_signature.declaration().segments().len() != 1 {
        return None;
    }
    let KlibIrType::Simple {
        classifier,
        nullability,
        arguments,
        ..
    } = arena.ty(expression.ty?)
    else {
        return None;
    };
    let KlibIrSignature::Public(classifier_signature) = &classifier.signature else {
        return None;
    };
    if classifier.kind != KlibIrSymbolKind::Class
        || *nullability == KlibIrNullability::MarkedNullable
    {
        return None;
    }
    let classifier_name = super::klib_types::classifier_name(classifier_signature).ok()?;
    let family = function_classifiers::classifier(classifier_name)?;
    if !family.is_reflective() || family.is_suspend() || arguments.len() != family.arity() + 1 {
        return None;
    }
    let KlibIrSignature::Public(invocation) = &overridden.signature else {
        return None;
    };
    if overridden.kind != KlibIrSymbolKind::Function
        || super::klib_types::member_owner(invocation).ok()? != classifier_name
        || super::klib_types::reference_invocation_arity(invocation) != Some(family.arity())
    {
        return None;
    }
    let function = arena.function(*invoke);
    if function.constructor
        || function.base.flags & crate::metadata::function_flags::IS_SUSPEND != 0
        || !function.type_parameters.is_empty()
        || function.dispatch_receiver.is_some()
        || !function.context_parameters.is_empty()
        || function.extension_receiver.is_some()
        || function.regular_parameters.len() != family.arity()
        || function.regular_parameters.iter().any(|parameter| {
            parameter.default_value.is_some() || parameter.vararg_element_type.is_some()
        })
    {
        return None;
    }
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        return None;
    };
    let [KlibIrStatement::Expression(returned)] = statements.as_slice() else {
        return None;
    };
    let KlibIrExprKind::Return {
        target: owner,
        value,
    } = &arena.expr(*returned).kind
    else {
        return None;
    };
    if owner != &function.base.symbol {
        return None;
    }
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &arena.expr(*value).kind
    else {
        return None;
    };
    if &access.symbol != target || !access.type_arguments.is_empty() {
        return None;
    }
    let values = match &access.arguments {
        KlibIrArguments::Flat(values)
        | KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => values,
        _ => return None,
    };
    if values.len() != function.regular_parameters.len() {
        return None;
    }
    for (value, parameter) in values.iter().zip(&function.regular_parameters) {
        if !matches!(&arena.expr((*value)?).kind,
            KlibIrExprKind::GetValue { symbol, .. } if symbol == &parameter.base.symbol)
        {
            return None;
        }
    }
    Some(UnboundReference {
        invoke: *invoke,
        target: target_signature,
        returned: *returned,
        arity: family.arity(),
    })
}

/// The declared function supertype of one exact reflective application. Nominal reflective
/// occurrences remain nominal; only this authenticated builtins-family projection is admitted.
pub(super) fn function_supertype(actual: Ty, expected: Ty) -> bool {
    matches!(expected.kind(), TyKind::Fun(signature)
        if !signature.suspend && !signature.has_receiver && signature.context_count == 0)
        && function_classifiers::function_reference_type(expected) == Some(actual)
}
