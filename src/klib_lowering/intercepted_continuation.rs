//! Kotlin/Wasm's `kotlin.wasm.internal.interceptedIntrinsic<T>(cont)`, the
//! `@ExcludedFromCodegen` body of `Continuation<T>.intercepted()`.
//!
//! kotlinc's `BuiltInsLowering` expands a call of it, in state-machine mode, to
//! `if (cont is CoroutineImpl) (cont as CoroutineImpl).intercepted() else cont` over the stdlib's
//! `kotlin.coroutines.CoroutineImpl`. Body lowering performs the same expansion: the member it
//! calls is an ordinary dependency declaration, which the closure roots wherever a body calls the
//! intrinsic and which lowering emits as it emits any member a body calls.

use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{
    CompilerIntrinsic, KlibBodyCallable, KlibDeclarationSignature, KlibDependencyCallable,
};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::KlibIrSignature;
use crate::types::{Ty, TyKind, TyParamName, TypeName};

use super::decline::KlibBodyDeclineReason;

/// The selected declaration's generic call shape: its one type parameter, and its parameter and
/// result in terms of it.
pub(crate) struct InterceptedContinuationCall {
    pub(super) formal: TyParamName,
    pub(super) params: Vec<Ty>,
    pub(super) result: Ty,
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

/// A call of the intrinsic with the exact member its expansion calls.
pub(crate) struct InterceptedContinuationExpansion {
    pub(super) call: InterceptedContinuationCall,
    pub(super) member: KlibPublicIdSignature,
}

/// The expansion of a call of `identity`, when its selected declaration is the intrinsic.
pub(super) fn expansion(
    callees: &super::KlibCalleeFacts<'_>,
    bodies: &KlibDeclarationBodies,
    identity: &KlibDependencyCallable,
) -> Result<Option<InterceptedContinuationExpansion>, KlibBodyDeclineReason> {
    let Some(call) = callees.intercepted_continuation(identity)? else {
        return Ok(None);
    };
    let KlibDeclarationSignature::Public(member) =
        intercepting_member(bodies, identity)?.declaration
    else {
        return Err(mismatch("the intercepting member has no public signature"));
    };
    Ok(Some(InterceptedContinuationExpansion { call, member }))
}

/// The state-machine coroutine class whose instances intercept themselves.
pub(super) fn coroutine_class() -> TypeName {
    crate::types::type_name("kotlin/coroutines/CoroutineImpl")
}

/// The call shape of `selected` when its provider authenticated it as the intrinsic; `None` for
/// any other declaration.
pub(super) fn authenticate(
    selected: KlibBodyCallable<'_>,
) -> Result<Option<InterceptedContinuationCall>, KlibBodyDeclineReason> {
    if selected.compiler_intrinsic() != Some(CompilerIntrinsic::InterceptedContinuation) {
        return Ok(None);
    }
    let generic = selected
        .generic_signature()
        .ok_or_else(|| mismatch("the continuation interception has no generic contract"))?;
    let [formal] = generic.formals.as_slice() else {
        return Err(mismatch(
            "the continuation interception does not have one type parameter",
        ));
    };
    if !selected.is_top_level() || selected.context_count() != 0 || generic.receiver.is_some() {
        return Err(mismatch(
            "the continuation interception has a different selected call shape",
        ));
    }
    let is_continuation = |ty: Ty| {
        matches!(ty.kind(), TyKind::Obj(owner, arguments)
            if owner.matches("kotlin/coroutines/Continuation")
                && matches!(arguments.as_slice(), &[argument]
                    if matches!(argument.kind(), TyKind::TyParam(name, _) if name == *formal)))
    };
    if !matches!(generic.params.as_slice(), &[param] if is_continuation(param))
        || !is_continuation(generic.ret)
    {
        return Err(mismatch(
            "the continuation interception has incompatible declared operand and result types",
        ));
    }
    Ok(Some(InterceptedContinuationCall {
        formal: *formal,
        params: generic.params.clone(),
        result: generic.ret,
    }))
}

/// The member a call of `selected` expands to call, when `selected` is the intrinsic. The checker's
/// member-edge walk and the emission closure both reach the member through this, never through
/// the intrinsic's excluded body.
pub(crate) fn expansion_member(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Option<KlibDependencyCallable>, KlibBodyDeclineReason> {
    if authenticate(selected)?.is_none() {
        return Ok(None);
    }
    intercepting_member(bodies, selected.identity()).map(Some)
}

/// The exact `CoroutineImpl.intercepted(): Continuation<Any?>` the expansion of a call of the
/// intrinsic `intrinsic` calls: the one direct member function of that name of the one
/// `CoroutineImpl` in the intrinsic's producer graph, without parameters or type parameters.
fn intercepting_member(
    bodies: &KlibDeclarationBodies,
    intrinsic: &KlibDependencyCallable,
) -> Result<KlibDependencyCallable, KlibBodyDeclineReason> {
    let class = KlibPublicIdSignature::classifier(coroutine_class());
    let (module, _, functions) = bodies
        .public_class_functions(&intrinsic.module, &class)
        .ok_or_else(|| mismatch("the continuation interception has no single coroutine class"))?;
    let members = functions
        .into_iter()
        .filter(|function| {
            function.name == "intercepted"
                && function.dispatch_receiver.is_some()
                && function.extension_receiver.is_none()
                && function.context_parameters.is_empty()
                && function.regular_parameters.is_empty()
                && function.type_parameters.is_empty()
        })
        .collect::<Vec<_>>();
    let [member] = members.as_slice() else {
        return Err(mismatch(
            "the coroutine class does not declare one intercepting member",
        ));
    };
    let KlibIrSignature::Public(signature) = &member.base.symbol.signature else {
        return Err(mismatch("the intercepting member has no public signature"));
    };
    Ok(KlibDependencyCallable {
        module,
        declaration: KlibDeclarationSignature::Public(signature.clone()),
    })
}
