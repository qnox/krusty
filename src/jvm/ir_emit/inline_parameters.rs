//! Which operands of a classpath inline call are inline parameters (kotlinc's
//! `InlineUtil.isInlineParameter`): a parameter whose declared type is a non-null function type
//! and that did not write `noinline`. A literal lambda passed anywhere else is an ordinary value,
//! even when the call's type arguments make the parameter a function type there (`with({ "" })
//! { … }` passes the lambda as the receiver `T`).

use crate::ir::{ExprId, IrFile};
use crate::types::{InlineParameterModifier, Ty};

/// Whether operand `parameter` of the realized inline call `call` is an inline parameter, from the
/// declaration's published modifiers and declared parameter types. `None` when the call published
/// either fact for no such operand.
pub(super) fn is_inline_parameter(ir: &IrFile, call: ExprId, parameter: usize) -> Option<bool> {
    let modifier = *ir.call_inline_modifiers.get(&call)?.get(parameter)?;
    let declared = *ir.call_declared_params.get(&call)?.get(parameter)?;
    Some(modifier != InlineParameterModifier::Noinline && matches!(declared, Ty::Fun(_)))
}
