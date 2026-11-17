//! Native's continuation intrinsics, which a KLIB body calls where kotlinc's pre-serialization
//! inliner expanded `suspendCoroutineUninterceptedOrReturn<T>(block)` into
//! `returnIfSuspended<T>(block(getContinuation<T>()))`.
//!
//! Together the two are that one suspension point: the body's own continuation handed to the
//! block, and the block's answer taken as the point's outcome. Body lowering realizes them with
//! the same common operations a source call of the suspension primitive lowers to, so the
//! coroutine preparation sees one kind of suspension point whichever form reached it.

use crate::libraries::{CompilerIntrinsic, KlibBodyCallable};
use crate::types::{Ty, TyKind, TyParamName};

use super::decline::KlibBodyDeclineReason;

/// Which of the two intrinsics a call selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContinuationIntrinsic {
    /// `getContinuation<T>()`: the enclosing suspend body's continuation.
    Current,
    /// `returnIfSuspended<T>(argument)`: the enclosing body suspends on `COROUTINE_SUSPENDED` and
    /// otherwise goes on with `argument` as its `T`.
    ReturnIfSuspended,
}

/// The selected declaration's generic call shape: its one type parameter, and its parameters and
/// result in terms of it.
pub(crate) struct ContinuationIntrinsicCall {
    pub(super) operation: ContinuationIntrinsic,
    pub(super) formal: TyParamName,
    pub(super) params: Vec<Ty>,
    pub(super) result: Ty,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// The continuation intrinsic `selected` is, from the operation its provider authenticated and
/// the generic contract it published; `None` for any other declaration.
pub(super) fn authenticate(
    selected: KlibBodyCallable<'_>,
) -> Result<Option<ContinuationIntrinsicCall>, KlibBodyDeclineReason> {
    let operation = match selected.compiler_intrinsic() {
        Some(CompilerIntrinsic::CurrentContinuation) => ContinuationIntrinsic::Current,
        Some(CompilerIntrinsic::ReturnIfSuspended) => ContinuationIntrinsic::ReturnIfSuspended,
        _ => return Ok(None),
    };
    let generic = selected
        .generic_signature()
        .ok_or_else(|| mismatch("a continuation intrinsic has no generic contract"))?;
    let [formal] = generic.formals.as_slice() else {
        return Err(mismatch(
            "a continuation intrinsic does not have one type parameter",
        ));
    };
    if !selected.is_top_level() || selected.context_count() != 0 || generic.receiver.is_some() {
        return Err(mismatch(
            "a continuation intrinsic has a different selected call shape",
        ));
    }
    let is_formal = |ty: Ty| matches!(ty.kind(), TyKind::TyParam(name, _) if name == *formal);
    let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    let valid = match operation {
        ContinuationIntrinsic::Current => {
            generic.params.is_empty()
                && matches!(generic.ret.kind(), TyKind::Obj(owner, arguments)
                    if owner.matches("kotlin/coroutines/Continuation")
                        && matches!(arguments.as_slice(), &[argument] if is_formal(argument)))
        }
        ContinuationIntrinsic::ReturnIfSuspended => {
            generic.params.as_slice() == [any] && is_formal(generic.ret)
        }
    };
    if !valid {
        return Err(mismatch(
            "a continuation intrinsic has incompatible declared operand and result types",
        ));
    }
    Ok(Some(ContinuationIntrinsicCall {
        operation,
        formal: *formal,
        params: generic.params.clone(),
        result: generic.ret,
    }))
}
