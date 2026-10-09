//! Complete semantic signatures realized by the Native coroutine runtime.
//!
//! The frontend has already selected every declaration. This table maps that stable declaration
//! shape to the corresponding runtime entry point; it performs no name lookup or overload choice.

use super::*;

/// Complete semantic identity of one selected property declaration, expressed by its getter.
/// `params` contains context parameters only; an extension receiver remains a receiver.
#[derive(Clone, Copy)]
pub(in crate::native) struct PropertySignature<'a> {
    pub(in crate::native) owner: DeclarationOwner,
    pub(in crate::native) name: &'a str,
    pub(in crate::native) receiver: Option<Ty>,
    pub(in crate::native) params: &'a [Ty],
    pub(in crate::native) ret: Ty,
}

impl<'a> PropertySignature<'a> {
    pub(in crate::native) fn new(
        owner: DeclarationOwner,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> Self {
        Self {
            owner,
            name,
            receiver: None,
            params,
            ret,
        }
    }

    pub(in crate::native) fn with_receiver(mut self, receiver: Option<Ty>) -> Self {
        self.receiver = receiver;
        self
    }
}

fn same_type(left: Ty, right: Ty) -> bool {
    left.canonical_semantic() == right.canonical_semantic()
}

fn classifier_argument(ty: Ty, classifier: &str) -> Option<Ty> {
    match ty.non_null() {
        Ty::Obj(owner, [argument]) if classifier_matches(owner, classifier) => Some(
            argument
                .projection_inner()
                .unwrap_or(*argument)
                .canonical_semantic(),
        ),
        _ => None,
    }
}

fn continuation_argument(ty: Ty) -> Option<Ty> {
    classifier_argument(ty, "kotlin/coroutines/Continuation")
}

fn result_argument(ty: Ty) -> Option<Ty> {
    classifier_argument(ty, "kotlin/Result")
}

fn nullable_any(ty: Ty) -> bool {
    ty.is_nullable()
        && ty
            .non_null()
            .obj_internal()
            .is_some_and(|owner| classifier_matches(owner, "kotlin/Any"))
}

fn nullable_classifier(ty: Ty, classifier: &str) -> bool {
    ty.is_nullable()
        && ty
            .non_null()
            .obj_internal()
            .is_some_and(|owner| classifier_matches(owner, classifier))
}

/// A coroutine library property the runtime answers: `COROUTINE_SUSPENDED` (no receiver) and a
/// `Continuation`'s `context` (its receiver the one operand).
pub(in crate::native) fn coroutine_property(
    signature: PropertySignature<'_>,
) -> Option<&'static str> {
    if signature
        .owner
        .package_matches("kotlin/coroutines/intrinsics")
        && signature.name == "COROUTINE_SUSPENDED"
        && signature.receiver.is_none()
        && signature.params.is_empty()
        && signature.ret == Ty::obj("kotlin/Any")
    {
        return Some("kt_coroutine_suspended");
    }
    if is_continuation_context_property(signature) {
        return Some("kt_continuation_context");
    }
    if signature.owner.classifier_matches("kotlin/Result")
        && signature.receiver.is_none()
        && signature.params.is_empty()
        && signature.ret == Ty::Boolean
    {
        return match signature.name {
            "isSuccess" => Some("kt_result_is_success"),
            "isFailure" => Some("kt_result_is_failure"),
            _ => None,
        };
    }
    None
}

/// Whether this exact declaration is `Continuation.context: CoroutineContext`.
pub(in crate::native) fn is_continuation_context_property(
    signature: PropertySignature<'_>,
) -> bool {
    signature
        .owner
        .classifier_matches("kotlin/coroutines/Continuation")
        && signature.name == "context"
        && signature.receiver.is_none()
        && signature.params.is_empty()
        && signature.ret == Ty::obj("kotlin/coroutines/CoroutineContext")
}

/// A call into the coroutine protocol or onto a `Result` that the runtime answers.
///
/// A `Result` crosses as its RAW value on this target (see `krusty_coroutines.c`), so every operand
/// and the answer travel as references without boxing a `Result` that is one of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native) struct CoroutineCall {
    pub(in crate::native) symbol: &'static str,
    /// Whether the receiver is a companion object nothing is read from.
    pub(in crate::native) companion_receiver: bool,
    /// What the runtime function answers: a reference, or nothing.
    pub(in crate::native) answers: bool,
    /// Whether it is a member of `Result`, whose dispatch receiver is the `Result` itself.
    pub(in crate::native) result_member: bool,
}

/// The runtime function realizing `signature`, when it is a coroutine entry point or a `Result`
/// member. Each suspend function, frame and suspend lambda is the program's own (see
/// `native::coroutines`); what is left are the library's few functions that START or RESUME one,
/// and the members a `Result` is read through. There is no dispatcher on this target, so
/// `intercepted` is the continuation itself.
pub(in crate::native) fn coroutine_call(signature: FunctionSignature<'_>) -> Option<CoroutineCall> {
    let result_member = signature.owner.classifier_matches("kotlin/Result");
    let call = |symbol, answers| {
        Some(CoroutineCall {
            symbol,
            companion_receiver: false,
            answers,
            result_member,
        })
    };
    let callback = |ty: Ty, parameter: Ty, result: Ty| {
        matches!(ty.non_null(), Ty::Fun(function)
            if !function.suspend
                && function.context_count == 0
                && !function.has_receiver
                && function.params.len() == 1
                && same_type(function.params[0], parameter)
                && same_type(function.ret, result))
    };
    let continuation_of = |ty: Ty, result: Ty| {
        continuation_argument(ty).is_some_and(|argument| same_type(argument, result))
    };
    let owner = signature.owner;
    // A suspend library function's signature ends in the continuation the caller passes it.
    if owner.package_matches("kotlin/coroutines") {
        if let ("Continuation", None, [context, resume]) =
            (signature.name, signature.receiver, signature.params)
        {
            let result = continuation_argument(signature.ret)?;
            let result_value = Ty::obj_args("kotlin/Result", &[result]);
            if *context == Ty::obj("kotlin/coroutines/CoroutineContext")
                && callback(*resume, result_value, Ty::Unit)
            {
                return call("kt_continuation_new", true);
            }
        }
        if signature.name == "suspendCoroutine" && signature.receiver.is_none() {
            let ([block] | [block, _]) = signature.params else {
                return None;
            };
            let hidden = signature
                .params
                .get(1)
                .is_none_or(|hidden| continuation_of(*hidden, signature.ret));
            if hidden
                && callback(
                    *block,
                    Ty::obj_args("kotlin/coroutines/Continuation", &[signature.ret]),
                    Ty::Unit,
                )
            {
                return call("kt_suspend_coroutine", true);
            }
        }
        if matches!(signature.name, "startCoroutine" | "createCoroutine") {
            let block = signature.receiver?;
            let Ty::Fun(function) = block.non_null() else {
                return None;
            };
            if !function.suspend
                || function.context_count != 0
                || function.params.len() > 1
                || function.has_receiver != !function.params.is_empty()
            {
                return None;
            }
            let completion = signature.params.last().copied()?;
            if signature.params.len() != function.params.len() + 1
                || !signature.params[..function.params.len()]
                    .iter()
                    .zip(&function.params)
                    .all(|(left, right)| same_type(*left, *right))
                || !continuation_of(completion, function.ret)
            {
                return None;
            }
            let creates = signature.name == "createCoroutine";
            if creates && !continuation_of(signature.ret, Ty::Unit)
                || !creates && signature.ret != Ty::Unit
            {
                return None;
            }
            return match (creates, function.params.is_empty()) {
                (false, true) => call("kt_start_coroutine", false),
                (false, false) => call("kt_start_coroutine_with_receiver", false),
                (true, true) => call("kt_create_coroutine", true),
                (true, false) => call("kt_create_coroutine_with_receiver", true),
            };
        }
        if signature.name == "resume" {
            let receiver = signature.receiver?;
            let [value] = signature.params else {
                return None;
            };
            if signature.ret == Ty::Unit
                && continuation_argument(receiver).is_some_and(|result| same_type(result, *value))
            {
                return call("kt_continuation_resume", false);
            }
        }
        if signature.name == "resumeWithException" {
            let receiver = signature.receiver?;
            if signature.params == [Ty::obj("kotlin/Throwable")]
                && signature.ret == Ty::Unit
                && continuation_argument(receiver).is_some()
            {
                return call("kt_continuation_resume_with_exception", false);
            }
        }
        return None;
    }
    if owner.package_matches("kotlin/coroutines/intrinsics") {
        if signature.name == "intercepted" && signature.params.is_empty() {
            let receiver = signature.receiver?;
            let argument = continuation_argument(receiver)?;
            if continuation_of(signature.ret, argument) {
                return call("kt_continuation_intercepted", true);
            }
        }
        if signature.name == "suspendCoroutineUninterceptedOrReturn" && signature.receiver.is_none()
        {
            let ([block] | [block, _]) = signature.params else {
                return None;
            };
            let hidden = signature
                .params
                .get(1)
                .is_none_or(|hidden| continuation_of(*hidden, signature.ret));
            if hidden
                && callback(
                    *block,
                    Ty::obj_args("kotlin/coroutines/Continuation", &[signature.ret]),
                    Ty::nullable(Ty::obj("kotlin/Any")),
                )
            {
                return call("kt_suspend_coroutine_unintercepted_or_return", true);
            }
        }
        if matches!(
            signature.name,
            "startCoroutineUninterceptedOrReturn" | "createCoroutineUnintercepted"
        ) {
            let block = signature.receiver?;
            let Ty::Fun(function) = block.non_null() else {
                return None;
            };
            if !function.suspend
                || function.context_count != 0
                || function.params.len() > 1
                || function.has_receiver != !function.params.is_empty()
            {
                return None;
            }
            let completion = signature.params.last().copied()?;
            if signature.params.len() != function.params.len() + 1
                || !signature.params[..function.params.len()]
                    .iter()
                    .zip(&function.params)
                    .all(|(left, right)| same_type(*left, *right))
                || !continuation_of(completion, function.ret)
            {
                return None;
            }
            let creates = signature.name == "createCoroutineUnintercepted";
            if creates && !continuation_of(signature.ret, Ty::Unit)
                || !creates && !nullable_any(signature.ret)
            {
                return None;
            }
            return match (creates, function.params.is_empty()) {
                (false, true) => call("kt_start_coroutine_unintercepted_or_return", true),
                (false, false) => call(
                    "kt_start_coroutine_unintercepted_or_return_with_receiver",
                    true,
                ),
                (true, true) => call("kt_create_coroutine", true),
                (true, false) => call("kt_create_coroutine_with_receiver", true),
            };
        }
        return None;
    }
    if owner.classifier_matches("kotlin/coroutines/Continuation") {
        if is_continuation_resume_with(signature) {
            return call("kt_continuation_resume_with", false);
        }
        return None;
    }
    // `getOrThrow` is an extension; `getOrNull` and `exceptionOrNull` are `Result`'s members, whose
    // receiver is the dispatch receiver.
    if owner.package_matches("kotlin") || result_member {
        if !result_member && signature.receiver.is_none() {
            return None;
        }
        if !signature.params.is_empty() {
            return None;
        }
        let result = signature.receiver.and_then(result_argument);
        return match signature.name {
            "getOrThrow" if result.is_some_and(|ty| same_type(ty, signature.ret)) => {
                call("kt_result_get_or_throw", true)
            }
            "getOrNull" if result.is_some_and(|ty| same_type(Ty::nullable(ty), signature.ret)) => {
                call("kt_result_get_or_null", true)
            }
            "exceptionOrNull" if nullable_classifier(signature.ret, "kotlin/Throwable") => {
                call("kt_result_exception_or_null", true)
            }
            _ if result_member
                && signature.name == "getOrThrow"
                && matches!(signature.ret, Ty::TyParam(_, _)) =>
            {
                call("kt_result_get_or_throw", true)
            }
            _ if result_member
                && signature.name == "getOrNull"
                && matches!(signature.ret, Ty::Nullable(inner) if matches!(*inner, Ty::TyParam(_, _))) =>
            {
                call("kt_result_get_or_null", true)
            }
            _ if result_member
                && signature.name == "exceptionOrNull"
                && nullable_classifier(signature.ret, "kotlin/Throwable") =>
            {
                call("kt_result_exception_or_null", true)
            }
            _ => None,
        };
    }
    if owner.classifier_matches("kotlin/Result$Companion") {
        if signature.receiver.is_some() {
            return None;
        }
        let result = result_argument(signature.ret)?;
        let symbol = match (signature.name, signature.params) {
            ("success", [value]) if same_type(*value, result) => "kt_result_success",
            ("failure", [error])
                if *error == Ty::obj("kotlin/Throwable") && matches!(result, Ty::TyParam(_, _)) =>
            {
                "kt_result_failure"
            }
            _ => return None,
        };
        return Some(CoroutineCall {
            symbol,
            companion_receiver: true,
            answers: true,
            result_member: false,
        });
    }
    None
}

/// Whether this exact declaration is `Continuation.resumeWith(Result<T>): Unit`.
pub(in crate::native) fn is_continuation_resume_with(signature: FunctionSignature<'_>) -> bool {
    signature
        .owner
        .classifier_matches("kotlin/coroutines/Continuation")
        && signature.name == "resumeWith"
        && signature.receiver.is_none()
        && signature.ret == Ty::Unit
        && matches!(signature.params, [result] if result_argument(*result).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str) -> DeclarationOwner {
        DeclarationOwner::classifier(crate::types::type_name(path))
    }

    fn package(path: &str) -> DeclarationOwner {
        let physical = crate::types::type_name(path);
        DeclarationOwner {
            physical,
            semantic: Some(SemanticCallableOwner::Package(physical)),
        }
    }

    fn function<'a>(
        owner: DeclarationOwner,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> FunctionSignature<'a> {
        FunctionSignature::new(owner, name, params, ret)
    }

    #[test]
    fn coroutine_member_intrinsics_require_the_complete_declaration_signature() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let continuation = Ty::obj_args("kotlin/coroutines/Continuation", &[parameter]);
        let result = Ty::obj_args("kotlin/Result", &[parameter]);
        let parameters = [result];
        let exact = function(
            member("kotlin/coroutines/Continuation"),
            "resumeWith",
            &parameters,
            Ty::Unit,
        );
        assert_eq!(
            coroutine_call(exact).map(|call| call.symbol),
            Some("kt_continuation_resume_with")
        );
        assert_eq!(
            coroutine_call(exact.with_receiver(Some(continuation))),
            None,
            "a member-extension receiver changes the selected declaration"
        );
        assert_eq!(
            coroutine_call(function(
                member("kotlin/coroutines/Continuation"),
                "resumeWith",
                &[Ty::String],
                Ty::Unit,
            )),
            None,
            "the Result<T> parameter is part of the intrinsic key"
        );
        assert_eq!(
            coroutine_call(function(
                member("kotlin/coroutines/Continuation"),
                "resumeWith",
                &parameters,
                Ty::Int,
            )),
            None,
            "the Unit result is part of the intrinsic key"
        );
    }

    #[test]
    fn coroutine_properties_require_the_complete_getter_signature() {
        let owner = member("kotlin/coroutines/Continuation");
        let context = Ty::obj("kotlin/coroutines/CoroutineContext");
        let exact = PropertySignature::new(owner, "context", &[], context);
        assert_eq!(coroutine_property(exact), Some("kt_continuation_context"));
        assert_eq!(
            coroutine_property(exact.with_receiver(Some(Ty::String))),
            None
        );
        assert_eq!(
            coroutine_property(PropertySignature::new(
                owner,
                "context",
                &[Ty::Int],
                context,
            )),
            None
        );
        assert_eq!(
            coroutine_property(PropertySignature::new(owner, "context", &[], Ty::String)),
            None
        );
    }

    #[test]
    fn start_coroutine_requires_matching_block_completion_and_result() {
        let result = Ty::ty_param("R", Ty::nullable(Ty::obj("kotlin/Any")));
        let block = Ty::fun_suspend(Vec::new(), result);
        let continuation = Ty::obj_args("kotlin/coroutines/Continuation", &[result]);
        let parameters = [continuation];
        let exact = function(
            package("kotlin/coroutines"),
            "startCoroutine",
            &parameters,
            Ty::Unit,
        )
        .with_receiver(Some(block));
        assert_eq!(
            coroutine_call(exact).map(|call| call.symbol),
            Some("kt_start_coroutine")
        );
        assert_eq!(
            coroutine_call(
                function(
                    package("kotlin/coroutines"),
                    "startCoroutine",
                    &[Ty::obj_args(
                        "kotlin/coroutines/Continuation",
                        &[Ty::String],
                    )],
                    Ty::Unit,
                )
                .with_receiver(Some(block)),
            ),
            None
        );
        assert_eq!(
            coroutine_call(
                function(
                    package("kotlin/coroutines"),
                    "startCoroutine",
                    &parameters,
                    Ty::String,
                )
                .with_receiver(Some(block)),
            ),
            None
        );
    }
}
