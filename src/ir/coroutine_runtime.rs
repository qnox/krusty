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
    /// `kotlin.Result.failure(Throwable)`, used to deliver an exception from a resumed body.
    pub result_failure: ExternalCallableId,
}
