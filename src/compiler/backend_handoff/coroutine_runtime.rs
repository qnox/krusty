//! Selects the stdlib declarations a suspend function's lowering calls (see
//! [`crate::ir::IrCoroutineRuntime`]) among the declarations the compilation sees.

use crate::ir::IrCoroutineRuntime;
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, SemanticCallableOwner};

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
        function.kind == FnKind::Extension
            && function
                .generic_sig
                .as_ref()
                .is_some_and(|signature| signature.formals.len() == 1)
            && function
                .receiver
                .and_then(|receiver| receiver.non_null().obj_internal())
                == Some(result)
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
        function.kind == FnKind::Member
            && function.semantic_receiver().is_none()
            && function.callable.declaration_owner
                == Some(SemanticCallableOwner::Classifier(companion))
            && function.semantic_params().len() == 1
            && function
                .generic_sig
                .as_ref()
                .is_some_and(|signature| signature.formals.len() == 1)
            && function.semantic_params()[0].non_null().obj_internal() == Some(throwable)
            && function
                .ret
                .apply(function.callable.ret)
                .non_null()
                .obj_internal()
                == Some(result)
    });
    let (Some(result_failure), None) = (result_failure.next(), result_failure.next()) else {
        return None;
    };
    let successes = companion_class.declared_callables.get("success")?;
    let mut result_success = successes.functions().iter().filter(|function| {
        function.kind == FnKind::Member
            && function.semantic_receiver().is_none()
            && function.callable.declaration_owner
                == Some(SemanticCallableOwner::Classifier(companion))
            && function.semantic_params().len() == 1
            && function
                .generic_sig
                .as_ref()
                .is_some_and(|signature| signature.formals.len() == 1)
            && function
                .ret
                .apply(function.callable.ret)
                .non_null()
                .obj_internal()
                == Some(result)
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
            && function.semantic_params()[0].non_null().obj_internal() == Some(result)
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
        context: context.getter.external_property_identity?,
        context_getter: context.getter.external_identity?,
        context_ty: context.ty,
        context_receiver_ty: crate::types::Ty::obj_name(continuation),
        result_success: result_success.callable.external_identity?,
        result_failure: result_failure.callable.external_identity?,
    })
}
