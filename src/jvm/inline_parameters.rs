//! Which operands of a classpath inline call are inline parameters (kotlinc's
//! `InlineUtil.isInlineParameter`): a parameter whose declared type is a non-null function type
//! and that did not write `noinline`. A literal lambda passed anywhere else is an ordinary value,
//! even when the call's type arguments make the parameter a function type there (`with({ "" })
//! { … }` passes the lambda as the receiver `T`).

use crate::ir::{ExprId, IrFile};
use crate::types::InlineParameterModifier;

/// Whether operand `parameter` of the realized inline call `call` is an inline parameter, from the
/// declaration's published modifiers and declared parameter types. `None` when the call published
/// either fact for no such operand.
pub(crate) fn is_inline_parameter(ir: &IrFile, call: ExprId, parameter: usize) -> Option<bool> {
    let modifier = *ir.call_inline_modifiers.get(&call)?.get(parameter)?;
    let declared = *ir.call_declared_params.get(&call)?.get(parameter)?;
    Some(modifier != InlineParameterModifier::Noinline && declared.is_fun())
}

/// Whether operand `parameter` of the realized inline call `call` is an inline parameter of a
/// suspend function type, whose function value, when the call does not inline it, the inlined
/// code invokes as a suspension point. `None` when the call published either fact for no such
/// operand.
pub(crate) fn is_inline_suspend_parameter(
    ir: &IrFile,
    call: ExprId,
    parameter: usize,
) -> Option<bool> {
    let declared = *ir.call_declared_params.get(&call)?.get(parameter)?;
    let suspend = matches!(declared.non_null().kind(), crate::types::TyKind::Fun(signature) if signature.suspend);
    Some(is_inline_parameter(ir, call, parameter)? && suspend)
}
