//! Expansion of an inline function's common-IR body where it is called.
//!
//! A call of an inline function is expanded by copying the function's lowered body into the call
//! site: the copy is specialized to the call's type arguments, its values are renumbered into the
//! caller's, and its returns leave a frame the expansion builds around it. Checked source calls and
//! calls in serialized dependency bodies both expand through these operations.

mod body_splice;
mod result_frame;
mod specialization;
mod value_numbering;

pub(crate) use body_splice::{
    fresh_value_base, splice_inline_body, InlineBodySplice, InlineBodySpliceFailure, InlineOperand,
    SplicedResult,
};
pub(crate) use result_frame::{
    frame_exit, leaves_inline_frame, produce_sole_tail_return, InlineReturn,
};
pub(crate) use specialization::specialize_inline_copy;
pub(crate) use value_numbering::{mark_subtree, rebase_values, value_indices};
