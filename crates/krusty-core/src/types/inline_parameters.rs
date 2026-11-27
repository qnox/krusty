//! Which parameters of an inline function are inline parameters (kotlinc's
//! `InlineUtil.isInlineParameter`): a parameter whose declared type is a non-null function type and
//! that did not write `noinline`. A lambda passed anywhere else is an ordinary function value.

use super::{InlineParameterModifier, Ty};

/// Whether a parameter that wrote `modifier` and declares `declared`, its type before the call's
/// substitution, is an inline parameter of its inline function.
pub fn is_inline_parameter(modifier: InlineParameterModifier, declared: Ty) -> bool {
    modifier != InlineParameterModifier::Noinline && declared.is_fun()
}
