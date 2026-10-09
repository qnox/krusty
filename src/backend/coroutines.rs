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
//! * [`hoisting`] lifts a suspension nested in an operand into a preceding temporary, in source
//!   evaluation order.
//!
//! They preserve checked Kotlin meaning and choose no representation. Where a rewrite has to name
//! one (the default a fresh temporary starts from, or the result type of a call a target pass has
//! already made physical), it asks the target through [`CoroutineRepresentation`].

pub(crate) mod block_splicing;
pub(crate) mod bottom_completion;
pub(crate) mod control_flow;
pub(crate) mod hoisting;
pub(crate) mod statement_normalization;
pub(crate) mod suspension_points;
pub(crate) mod value_namespace;
pub(crate) mod value_try;
pub(crate) mod value_when;

use crate::ir::{Callee, IrConst};
use crate::types::Ty;

/// The representation facts a target supplies to the shared suspend normalizations.
pub(crate) trait CoroutineRepresentation {
    /// The constant a fresh temporary of type `ty` starts from before its first real assignment.
    fn zero(&self, ty: &Ty) -> IrConst;

    /// The result type of a call whose callee a target pass has already made physical, such as a
    /// descriptor-carrying static or virtual call. `None` declines to snapshot it.
    fn physical_call_result(&self, callee: &Callee) -> Option<Ty>;

    /// The value type of a dependency static field read, from the physical field identity the
    /// target recorded. `None` declines to snapshot it.
    fn static_field_type(&self, descriptor: &str) -> Option<Ty>;

    /// The type of a class-literal constant (`Foo::class.java`).
    fn class_constant_type(&self) -> Ty;
}

/// What hoisting needs to type the temporaries it introduces.
pub(crate) struct SuspensionTyping<'a> {
    /// Every function's declared (pre-CPS) result, indexed by `FunId`.
    pub(crate) orig_rets: &'a [Ty],
    pub(crate) representation: &'a dyn CoroutineRepresentation,
}
