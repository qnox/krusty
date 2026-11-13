//! Selects the stdlib declarations a suspend function's lowering calls (see
//! [`crate::ir::IrCoroutineRuntime`]) among the declarations the compilation sees.

use crate::ir::IrCoroutineRuntime;
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, SemanticCallableOwner, Ty, TyKind, TyParamName};

#[cfg(test)]
mod tests;

/// The runtime of a file that declares a suspend function, or `None` when the classpath does not
/// publish the exact marker, completion, and continuation method declarations this lowering uses.
pub(super) fn select(source: &dyn SymbolSource) -> Option<IrCoroutineRuntime> {
    let intrinsics = source.symbols(
        SymbolNamespace::Package(type_name("kotlin/coroutines/intrinsics")),
        "COROUTINE_SUSPENDED",
    );
    let mut suspended = intrinsics
        .callables
        .properties()
        .iter()
        .filter(|property| property.kind == PropKind::TopLevel && property.receiver.is_none());
    let (Some(suspended), None) = (suspended.next(), suspended.next()) else {
        return None;
    };
    let kotlin = source.symbols(SymbolNamespace::Package(type_name("kotlin")), "getOrThrow");
    let result = type_name("kotlin/Result");
    let mut get_or_throw = kotlin.callables.functions().iter().filter(|function| {
        let Some(signature) = function.generic_sig.as_ref() else {
            return false;
        };
        let [formal] = signature.formals.as_slice() else {
            return false;
        };
        function.kind == FnKind::Extension
            && function.context_count == 0
            && function.semantic_params().is_empty()
            && signature.params.is_empty()
            && function
                .semantic_receiver()
                .is_some_and(|receiver| is_result_of(receiver, result, *formal))
            && signature
                .receiver
                .is_some_and(|receiver| is_result_of(receiver, result, *formal))
            && is_formal(signature.ret, *formal)
    });
    let (Some(get_or_throw), None) = (get_or_throw.next(), get_or_throw.next()) else {
        return None;
    };

    let result_class = source.classifier(result)?;
    let companion = result_class.companion_object.as_ref()?.1;
    let companion_class = source.classifier(companion)?;
    let throwable = type_name("kotlin/Throwable");
    let failures = companion_class.declared_callables.get("failure")?;
    let mut result_failure = failures.functions().iter().filter(|function| {
        let Some(signature) = function.generic_sig.as_ref() else {
            return false;
        };
        let [formal] = signature.formals.as_slice() else {
            return false;
        };
        function.kind == FnKind::Member
            && function.context_count == 0
            && function.semantic_receiver().is_none()
            && function.callable.declaration_owner
                == Some(SemanticCallableOwner::Classifier(companion))
            && function.semantic_params().as_ref() == signature.params.as_slice()
            && signature.params.len() == 1
            && signature.params[0] == signature.params[0].non_null()
            && signature.params[0].non_null().obj_internal() == Some(throwable)
            && is_result_of(signature.ret, result, *formal)
    });
    let (Some(result_failure), None) = (result_failure.next(), result_failure.next()) else {
        return None;
    };
    let successes = companion_class.declared_callables.get("success")?;
    let mut result_success = successes.functions().iter().filter(|function| {
        let Some(signature) = function.generic_sig.as_ref() else {
            return false;
        };
        let [formal] = signature.formals.as_slice() else {
            return false;
        };
        function.kind == FnKind::Member
            && function.context_count == 0
            && function.semantic_receiver().is_none()
            && function.callable.declaration_owner
                == Some(SemanticCallableOwner::Classifier(companion))
            && function.semantic_params().as_ref() == signature.params.as_slice()
            && signature.params.len() == 1
            && is_formal(signature.params[0], *formal)
            && is_result_of(signature.ret, result, *formal)
    });
    let (Some(result_success), None) = (result_success.next(), result_success.next()) else {
        return None;
    };

    let continuation = type_name("kotlin/coroutines/Continuation");
    let continuation_class = source.classifier(continuation)?;
    let resume_with = continuation_class.declared_callables.get("resumeWith")?;
    let mut declarations = resume_with.functions().iter().filter(|function| {
        function.kind == FnKind::Member
            && function.semantic_receiver().is_none()
            && function.callable.declaration_owner
                == Some(SemanticCallableOwner::Classifier(continuation))
            && function.semantic_params().len() == 1
            && is_result_application(function.semantic_params()[0], result)
            && function.ret.apply(function.callable.ret).non_null() == crate::types::Ty::Unit
    });
    let (Some(resume_with), None) = (declarations.next(), declarations.next()) else {
        return None;
    };

    let context_name = type_name("kotlin/coroutines/CoroutineContext");
    let context = continuation_class.declared_callables.get("context")?;
    let mut context_declarations = context.properties().iter().filter(|property| {
        property.kind == PropKind::Member
            && property.receiver.is_none()
            && property.owner == continuation
            && property.ty.non_null().obj_internal() == Some(context_name)
    });
    let (Some(context), None) = (context_declarations.next(), context_declarations.next()) else {
        return None;
    };

    Some(IrCoroutineRuntime {
        suspended: suspended.getter.external_property_identity?,
        get_or_throw: get_or_throw.callable.external_identity?,
        resume_with: resume_with.callable.external_identity?,
        resume_with_declared_parameter: resume_with.semantic_params().first().copied()?,
        resume_with_declared_result: resume_with.ret.apply(resume_with.callable.ret),
        context: context.getter.external_property_identity?,
        context_getter: context.getter.external_identity?,
        context_ty: context.ty,
        context_receiver_ty: crate::types::Ty::obj_name(continuation),
        result_success: result_success.callable.external_identity?,
        result_companion: companion,
        result_failure: result_failure.callable.external_identity?,
    })
}

fn is_formal(ty: Ty, formal: TyParamName) -> bool {
    matches!(ty.kind(), TyKind::TyParam(name, _) if name == formal)
}

fn is_result_of(ty: Ty, result: crate::types::TypeName, formal: TyParamName) -> bool {
    if ty != ty.non_null() {
        return false;
    }
    matches!(ty.kind(), TyKind::Obj(name, arguments)
        if name == result && arguments.len() == 1 && is_formal(arguments[0], formal))
}

fn is_result_application(ty: Ty, result: crate::types::TypeName) -> bool {
    ty == ty.non_null()
        && matches!(ty.kind(), TyKind::Obj(name, arguments)
            if name == result && arguments.len() == 1)
}
