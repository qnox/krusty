//! Declarations selected for suspend lowering's coroutine runtime operations.

use crate::ir::IrCoroutineRuntime;
use crate::libraries::{FnKind, PropKind};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{type_name, SemanticCallableOwner, Ty, TyKind};

#[cfg(test)]
mod tests;

/// The dependency declarations coroutine lowering may need for this compilation.
pub(super) fn selected(source: &dyn SymbolSource) -> Option<IrCoroutineRuntime> {
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

    let result = type_name("kotlin/Result");
    let throwable = type_name("kotlin/Throwable");
    let throws = source.symbols(
        SymbolNamespace::Package(type_name("kotlin")),
        "throwOnFailure",
    );
    let mut throw_on_failure = throws.callables.functions().iter().filter(|function| {
        function.kind == FnKind::Extension
            && function.context_count == 0
            && function.semantic_params().is_empty()
            && function
                .semantic_receiver()
                .is_some_and(|receiver| is_result_application(receiver, result))
            && function.ret.apply(function.callable.ret) == Ty::Unit
    });
    let (Some(throw_on_failure), None) = (throw_on_failure.next(), throw_on_failure.next()) else {
        return None;
    };
    let failures = source.symbols(
        SymbolNamespace::Package(type_name("kotlin")),
        "createFailure",
    );
    let mut create_failure = failures.callables.functions().iter().filter(|function| {
        function.kind == FnKind::TopLevel
            && function.context_count == 0
            && function.semantic_receiver().is_none()
            && function.semantic_params().len() == 1
            && function.semantic_params()[0] == function.semantic_params()[0].non_null()
            && function.semantic_params()[0].obj_internal() == Some(throwable)
            && function.ret.apply(function.callable.ret) == Ty::obj_name(crate::types::wk::any())
    });
    let (Some(create_failure), None) = (create_failure.next(), create_failure.next()) else {
        return None;
    };

    let result_class = source.classifier(result)?;
    let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    let mut constructors = result_class
        .constructors
        .iter()
        .filter(|constructor| constructor.params.as_slice() == [any]);
    let (Some(result_constructor), None) = (constructors.next(), constructors.next()) else {
        return None;
    };
    let values = result_class.declared_callables.get("value")?;
    let mut result_value = values.properties().iter().filter(|property| {
        property.kind == PropKind::Member
            && property.receiver.is_none()
            && property.owner == result
            && property.ty == any
    });
    let (Some(result_value), None) = (result_value.next(), result_value.next()) else {
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
        throw_on_failure: throw_on_failure.callable.external_identity?,
        create_failure: create_failure.callable.external_identity?,
        result_constructor: result_constructor.external_identity?,
        result_value: result_value.getter.external_property_identity?,
        resume_with: resume_with.callable.external_identity?,
        resume_with_declared_parameter: resume_with.semantic_params().first().copied()?,
        resume_with_declared_result: resume_with.ret.apply(resume_with.callable.ret),
        context: context.getter.external_property_identity?,
        context_getter: context.getter.external_identity?,
        context_ty: context.ty,
        context_receiver_ty: crate::types::Ty::obj_name(continuation),
    })
}

fn is_result_application(ty: Ty, result: crate::types::TypeName) -> bool {
    ty == ty.non_null()
        && matches!(ty.kind(), TyKind::Obj(name, arguments)
            if name == result && arguments.len() == 1)
}
