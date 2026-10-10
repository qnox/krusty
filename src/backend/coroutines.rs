//! Target-neutral coroutine lowering: the normalizations every suspend-function lowering runs before
//! it splits a body at its suspension points.
//!
//! Common lowering keeps a `suspend fun` plain and records it in `IrFile::suspend_funs`. A target's
//! coroutine pass then realizes suspension. Whatever machine it builds, it first needs every
//! suspension point at a statement boundary, as a direct statement, a local's initializer, or an
//! assignment's value. The passes here do that rewrite on common IR:
//!
//! * [`suspension_points`] decides which expressions suspend;
//! * [`block_splicing`], [`statement_normalization`], [`value_try`] and [`value_when`] move a
//!   suspension out of a value-position block, `try` or `when` into statements of their own;
//! * [`finally_linearization`] turns a `finally` into a catch, the cleanup, and a rethrow;
//! * [`loop_conditions`] tests a suspending loop condition inside the loop's body;
//! * [`hoisting`] lifts a suspension nested in an operand into a preceding temporary, in source
//!   evaluation order.
//!
//! They preserve checked Kotlin meaning and choose no representation. A temporary takes the type
//! recorded for the value it holds, never one these passes derive; the constant a fresh temporary
//! starts from is the target's, asked through [`CoroutineRepresentation`].

mod block_splicing;
mod bottom_completion;
mod control_flow;
mod finally_linearization;
mod hoisting;
mod loop_conditions;
mod operand_suspensions;
mod state_machine;
mod statement_normalization;
mod suspension_points;
mod value_namespace;
mod value_try;
mod value_when;

pub(crate) use block_splicing::{diverging_control_core, splice_return_blocks};
pub(crate) use bottom_completion::{
    suspension_completion, unwrap_suspend_cast, SuspensionCompletion,
};
pub(crate) use control_flow::{expr_has_return, stmt_diverges};
pub(crate) use finally_linearization::{
    linearize_finally_returns, linearize_suspending_finally, separate_catches_from_finally,
    FinallySuspension,
};
pub(crate) use hoisting::{hoist_spliced_inline_bodies, hoist_suspensions};
pub(crate) use loop_conditions::test_suspending_conditions_in_body;
pub(crate) use operand_suspensions::hoist_operand_suspensions;
pub(crate) use state_machine::{build_state_machine, CoroutineAbi, MachineFrame, MachineInput};
pub(crate) use statement_normalization::{
    desugar_tail_suspend, normalize_block_inits, normalize_statement_try_results,
    promote_diverging_tail_to_statement, split_unit_conditional_returns,
};
pub(crate) use suspension_points::{
    count_suspensions, expr_calls_suspend, is_suspension_point, recorded_suspension_result,
    suspend_call_fid, value_class_suspension_result,
};
pub(crate) use value_namespace::{
    function_value_types, is_rematerialized_null, max_value_index, zero_value,
};
pub(crate) use value_try::desugar_value_try;
pub(crate) use value_when::desugar_value_when;

use crate::ir::IrConst;
use crate::types::{Ty, TypeName};

/// The representation facts a target supplies to the shared suspend normalizations.
pub(crate) trait CoroutineRepresentation {
    /// The constant a fresh temporary of type `ty` starts from before its first real assignment.
    fn zero(&self, ty: &Ty) -> IrConst;

    /// The classifier a handler names to catch every exception.
    fn throwable(&self) -> TypeName;
}

/// What hoisting needs to type the temporaries it introduces.
pub(crate) struct SuspensionTyping<'a> {
    /// Every function's declared (pre-CPS) result, indexed by `FunId`.
    pub(crate) orig_rets: &'a [Ty],
    pub(crate) representation: &'a dyn CoroutineRepresentation,
}
