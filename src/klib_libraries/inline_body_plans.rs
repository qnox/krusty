//! Inline body plans decoded from an inline function's serialized KLIB body.
//!
//! An inline declaration whose whole body invokes one of its function-typed parameters, and then
//! returns either that invocation's result or one of its own parameters, is the same
//! source-independent contract the JVM provider decodes from a compiled inline body
//! ([`InlineBodyPlan::InvokeLambda`]). Publishing it here lets the checker and common lowering
//! expand a call of `let`, `run`, `with`, `also` or `apply` exactly as they expand the JVM
//! declaration. A body of any other shape publishes no plan and stays an ordinary call.

use crate::libraries::{function_classifiers, InlineBodyFrame, InlineBodyPlan, InlineBodyValue};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrArguments, KlibIrBody, KlibIrExprId, KlibIrExprKind, KlibIrFunction,
    KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::metadata::semantic::KotlinFunction;
use crate::types::type_name;

/// The plan of `function`'s body, when its metadata `declaration` is an inline function whose
/// body has the invoke shape.
pub(super) fn decode(
    declaration: &KotlinFunction,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Option<InlineBodyPlan> {
    if !declaration.is_inline
        || declaration.is_suspend
        || declaration.context_count != 0
        || function.dispatch_receiver.is_some()
        || !function.context_parameters.is_empty()
    {
        return None;
    }
    // Plan ordinals count the extension receiver as parameter 0, then the value parameters.
    let parameters = function
        .extension_receiver
        .iter()
        .chain(&function.regular_parameters)
        .collect::<Vec<_>>();
    let ordinal = |symbol: &KlibIrSymbol| {
        parameters
            .iter()
            .position(|parameter| parameter.base.symbol == *symbol)
    };
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        return None;
    };
    let (invocation, result) = match statements.as_slice() {
        [KlibIrStatement::Expression(returned)] => {
            (returned_value(arena, function, *returned)?, None)
        }
        [KlibIrStatement::Expression(invocation), KlibIrStatement::Expression(returned)] => {
            let returned = returned_value(arena, function, *returned)?;
            let KlibIrExprKind::GetValue { symbol, .. } = &arena.expr(returned).kind else {
                return None;
            };
            (
                *invocation,
                Some(InlineBodyValue::Parameter(ordinal(symbol)?)),
            )
        }
        _ => return None,
    };
    let (lambda, operands) = function_invocation(arena, invocation)?;
    let lambda_parameter = ordinal(lambda)?;
    let arguments = operands
        .iter()
        .map(|operand| match &arena.expr(*operand).kind {
            KlibIrExprKind::GetValue { symbol, .. } => ordinal(symbol)
                .filter(|parameter| *parameter != lambda_parameter)
                .map(InlineBodyValue::Parameter),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(InlineBodyPlan::InvokeLambda {
        frame: InlineBodyFrame {
            callee: function.name.as_str().into(),
            inline_only: function
                .base
                .annotations
                .iter()
                .any(|annotation| is_inline_only_constructor(&annotation.access.symbol)),
            source: None,
        },
        lambda_parameter,
        arguments,
        prologue: Vec::new(),
        cleanup: Vec::new(),
        cause: None,
        recovery: None,
        defaults: Vec::new(),
        result,
    })
}

/// The value `id` returns from `function` itself.
fn returned_value(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    id: KlibIrExprId,
) -> Option<KlibIrExprId> {
    match &arena.expr(id).kind {
        KlibIrExprKind::Return { target, value } if *target == function.base.symbol => Some(*value),
        _ => None,
    }
}

/// The function value parameter `id` invokes, and the operands it passes, when `id` is an
/// `invoke` of a language function type on a parameter read.
fn function_invocation(
    arena: &KlibIrArena,
    id: KlibIrExprId,
) -> Option<(&KlibIrSymbol, Vec<KlibIrExprId>)> {
    let KlibIrExprKind::Call { access, .. } = &arena.expr(id).kind else {
        return None;
    };
    if access.origin.as_deref() != Some("INVOKE") || !access.type_arguments.is_empty() {
        return None;
    }
    let (receiver, values): (_, &[Option<KlibIrExprId>]) = match &access.arguments {
        KlibIrArguments::Flat(arguments) => {
            let (receiver, values) = arguments.split_first()?;
            (*receiver, values)
        }
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver: None,
            values,
        } => (*dispatch_receiver, values),
        KlibIrArguments::Split { .. } => return None,
    };
    let KlibIrExprKind::GetValue { symbol, .. } = &arena.expr(receiver?).kind else {
        return None;
    };
    let operands = values.iter().copied().collect::<Option<Vec<_>>>()?;
    is_function_type_invoke(&access.symbol, operands.len()).then_some((symbol, operands))
}

/// `kotlin.FunctionN.invoke` with exactly `operands` value parameters: the member an ordinary
/// (non-suspend) function type's invocation calls.
fn is_function_type_invoke(symbol: &KlibIrSymbol, operands: usize) -> bool {
    let KlibIrSignature::Public(signature) = &symbol.signature else {
        return false;
    };
    let [classifier, member] = signature.declaration().segments() else {
        return false;
    };
    symbol.kind == KlibIrSymbolKind::Function
        && member == "invoke"
        && signature.package().segments() == ["kotlin"]
        && function_classifiers::classifier_name_in(type_name("kotlin"), classifier)
            .and_then(function_classifiers::classifier)
            .is_some_and(|function| {
                !function.is_suspend() && !function.is_reflective() && function.arity() == operands
            })
}

/// The constructor of `kotlin.internal.InlineOnly`.
fn is_inline_only_constructor(symbol: &KlibIrSymbol) -> bool {
    let KlibIrSignature::Public(signature) = &symbol.signature else {
        return false;
    };
    symbol.kind == KlibIrSymbolKind::Constructor
        && signature.package().segments() == ["kotlin", "internal"]
        && signature.declaration().segments() == ["InlineOnly", "<init>"]
}
