//! The coroutine-intrinsic registry — the FQ-name table behind [`crate::libraries::CoroutineIntrinsic`].
//!
//! `suspendCoroutineUninterceptedOrReturn`, `COROUTINE_SUSPENDED`, `startCoroutine`, `createCoroutine` are
//! real `@InlineOnly` stdlib declarations (in `kotlin/coroutines/intrinsics/IntrinsicsKt` +
//! `kotlin/coroutines/ContinuationKt`) whose stub bodies just `throw` — they exist only to give source a
//! type-checkable symbol. kotlinc never inlines or `invokestatic`s them; it replaces each by FQ name with
//! dedicated bytecode (the shape `javap` shows for a kotlinc-compiled coroutine). krusty's splice gate
//! (`can_inline_call`) correctly refuses the `throw` body, so without this table the calls fall through to
//! "unresolved". Only names are listed here; *signatures* come from `@Metadata` and *codegen* lives in the
//! lowering layer keyed on the [`CoroutineIntrinsic`] variant.

use crate::libraries::CoroutineIntrinsic;

/// Recognize a `kotlin.coroutines` intrinsic by its *unqualified* source name. Used by the resolver
/// (through [`crate::libraries::LibrarySet::coroutine_intrinsic`]) and the JVM lowering when an
/// unqualified call/reference reaches them under a `kotlin.coroutines[.intrinsics].*` import.
pub fn recognize_unqualified(name: &str) -> Option<CoroutineIntrinsic> {
    match name {
        "COROUTINE_SUSPENDED" => Some(CoroutineIntrinsic::CoroutineSuspended),
        "suspendCoroutineUninterceptedOrReturn" => {
            Some(CoroutineIntrinsic::SuspendCoroutineUninterceptedOrReturn)
        }
        "startCoroutine" => Some(CoroutineIntrinsic::StartCoroutine),
        "createCoroutine" => Some(CoroutineIntrinsic::CreateCoroutine),
        _ => None,
    }
}
