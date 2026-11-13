//! The stdlib declarations a suspend function's lowering calls, selected once by the handoff.
//!
//! A target's suspend lowering reads `COROUTINE_SUSPENDED` after every call that may suspend and
//! rethrows a failed resume with `Result.getOrThrow`, whether or not the source names either. A
//! lowering that runs after checking cannot select a dependency declaration, so the handoff does:
//! it selects both through the compilation's symbol source and records their identities here, as it
//! does for the runtime declarations a plugin's generated code calls.

use crate::fir::{ExternalCallableId, ExternalPropertyId};

/// The dependency declarations the coroutine lowering of a file calls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrCoroutineRuntime {
    /// `kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED`.
    pub suspended: ExternalPropertyId,
    /// `kotlin.Result<T>.getOrThrow()`.
    pub get_or_throw: ExternalCallableId,
    /// `kotlin.coroutines.Continuation.resumeWith(Result<Any?>)`.
    pub resume_with: ExternalCallableId,
    /// The provider declaration's un-applied `resumeWith` parameter type.
    pub resume_with_declared_parameter: crate::types::Ty,
    /// The provider declaration's un-applied `resumeWith` result type.
    pub resume_with_declared_result: crate::types::Ty,
    /// The exact `kotlin.coroutines.Continuation.context` property.
    pub context: ExternalPropertyId,
    /// The exact getter callable used by the Continuation property override contract.
    pub context_getter: ExternalCallableId,
    /// The declared type of `Continuation.context`.
    pub context_ty: crate::types::Ty,
    /// The selected property's receiver type, retained for its checked read.
    pub context_receiver_ty: crate::types::Ty,
    /// `kotlin.Result.Companion.success(T)`, used to complete the source continuation.
    pub result_success: ExternalCallableId,
    /// The provider-authenticated `Result.Companion` classifier owning success and failure.
    pub result_companion: crate::types::TypeName,
    /// `kotlin.Result.failure(Throwable)`, used to deliver an exception from a resumed body.
    pub result_failure: ExternalCallableId,
}
