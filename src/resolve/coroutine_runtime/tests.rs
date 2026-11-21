use super::*;
use crate::backend::CheckedBackendCallables;
use crate::compilation_target::CompilationTarget;
use crate::ir::{IrCheckedOperation, IrExpr, IrFile, IrFunction, IrPropertyDispatch};
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use crate::libraries::{ExternalCallableKind, KlibDependencyCallable};
use crate::types::Ty;

#[test]
fn selected_runtime_declarations_keep_exact_provider_identities_without_inventing_uses() {
    let mut checked = 0;
    for (target, semantics) in [
        (CompilationTarget::Native, KlibStdlibSemantics::Native),
        (CompilationTarget::WasmJs, KlibStdlibSemantics::Wasm),
        (CompilationTarget::WasmWasi, KlibStdlibSemantics::Wasm),
    ] {
        let Some(stdlib) = crate::toolchain::kotlin_stdlib_klib(target) else {
            assert!(
                std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
                "the exact {target:?} stdlib is required"
            );
            continue;
        };
        checked += 1;
        let provider = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(stdlib, semantics)])
            .expect("open the configured target stdlib");
        assert_selected_runtime(&provider);
    }
    assert!(checked > 0, "no target stdlib KLIB is provisioned");
}

fn assert_selected_runtime(provider: &KlibLibraries) {
    let runtime = selected(provider).expect("one exact coroutine runtime");
    let property = provider
        .external_property(runtime.suspended)
        .expect("the marker is the selected provider property");
    assert_eq!(property.name, "COROUTINE_SUSPENDED");
    let marker = provider.external_callable(property.getter).unwrap();
    assert_provider_identity(provider, property.getter);
    assert_eq!(
        marker.callable.declaration_owner,
        Some(SemanticCallableOwner::Package(type_name(
            "kotlin/coroutines/intrinsics"
        )))
    );

    let throw_on_failure = provider
        .external_callable(runtime.throw_on_failure)
        .unwrap();
    let throw_on_failure_identity = assert_provider_identity(provider, runtime.throw_on_failure);
    assert_eq!(throw_on_failure.kind, ExternalCallableKind::Extension);
    assert_eq!(throw_on_failure.callable.name, "throwOnFailure");
    assert_eq!(
        throw_on_failure
            .callable
            .source_receiver
            .map(Ty::non_null)
            .and_then(Ty::obj_internal),
        Some(type_name("kotlin/Result"))
    );
    assert_eq!(throw_on_failure.callable.ret, Ty::Unit);
    assert_eq!(
        Some(runtime.throw_on_failure_receiver),
        throw_on_failure.callable.source_receiver
    );

    let create_failure = provider.external_callable(runtime.create_failure).unwrap();
    let create_failure_identity = assert_provider_identity(provider, runtime.create_failure);
    assert_eq!(create_failure.kind, ExternalCallableKind::TopLevel);
    assert_eq!(create_failure.callable.name, "createFailure");
    assert_eq!(
        create_failure.callable.declaration_owner,
        Some(SemanticCallableOwner::Package(type_name("kotlin")))
    );
    assert_eq!(create_failure.callable.params.len(), 1);
    assert_eq!(
        create_failure.callable.params[0].non_null().obj_internal(),
        Some(type_name("kotlin/Throwable"))
    );

    let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    let constructor = provider
        .external_callable(runtime.result_constructor)
        .unwrap();
    let constructor_identity = assert_provider_identity(provider, runtime.result_constructor);
    assert_eq!(constructor.kind, ExternalCallableKind::Constructor);
    assert_eq!(
        constructor.callable.declaration_owner,
        Some(SemanticCallableOwner::Classifier(type_name(
            "kotlin/Result"
        )))
    );
    assert_eq!(constructor.callable.params, [any]);

    let value = provider.external_property(runtime.result_value).unwrap();
    assert_eq!(value.name, "value");
    let value_getter = provider.external_callable(value.getter).unwrap();
    assert_provider_identity(provider, value.getter);
    assert_eq!(value_getter.callable.ret, any);

    let resume_with = provider.external_callable(runtime.resume_with).unwrap();
    let resume_with_identity = assert_provider_identity(provider, runtime.resume_with);
    assert_eq!(resume_with.kind, ExternalCallableKind::Member);
    assert_eq!(resume_with.callable.name, "resumeWith");
    assert_eq!(
        resume_with.callable.declaration_owner,
        Some(SemanticCallableOwner::Classifier(type_name(
            "kotlin/coroutines/Continuation"
        )))
    );
    assert_eq!(resume_with.callable.params.len(), 1);
    assert_eq!(
        runtime.resume_with_declared_parameter,
        resume_with.callable.params[0]
    );
    assert_eq!(
        runtime.resume_with_declared_result,
        resume_with.callable.ret
    );
    assert_eq!(
        resume_with.callable.params[0].non_null().obj_internal(),
        Some(type_name("kotlin/Result"))
    );
    assert_eq!(resume_with.callable.ret, Ty::Unit);

    let context = provider.external_property(runtime.context).unwrap();
    assert_eq!(context.name, "context");
    assert_eq!(context.getter, runtime.context_getter);
    let context_getter = provider.external_callable(runtime.context_getter).unwrap();
    assert_eq!(
        context_getter.callable.ret.non_null().obj_internal(),
        Some(type_name("kotlin/coroutines/CoroutineContext"))
    );
    assert_provider_identity(provider, runtime.context_getter);
    assert_eq!(context_getter.kind, ExternalCallableKind::Member);
    assert_eq!(runtime.context_ty, context_getter.callable.ret);
    assert_eq!(
        runtime.context_receiver_ty.non_null().obj_internal(),
        Some(type_name("kotlin/coroutines/Continuation"))
    );

    let identities = [
        throw_on_failure_identity,
        create_failure_identity,
        constructor_identity,
        resume_with_identity,
    ];
    for (index, identity) in identities.iter().enumerate() {
        assert!(!identities[index + 1..].contains(identity));
    }

    assert_only_generated_uses_are_frozen(provider, runtime);
}

fn assert_only_generated_uses_are_frozen(provider: &KlibLibraries, runtime: IrCoroutineRuntime) {
    let property = provider.external_property(runtime.suspended).unwrap();
    let marker = provider.external_callable(property.getter).unwrap();
    let marker_identity = assert_provider_identity(provider, property.getter);

    // Selection supplies declaration evidence. It does not make a marker read, completion call,
    // or failure delivery executable, and must not add any of them to the frozen IR inventory.
    let mut ir = IrFile::default();
    ir.coroutine_runtime = Some(runtime);
    let frozen = CheckedBackendCallables::freeze(&ir, provider).unwrap();
    assert!(frozen.property(runtime.suspended).is_none());
    assert!(frozen.property(runtime.context).is_none());
    assert!(frozen.property(runtime.result_value).is_none());
    for target in [
        runtime.throw_on_failure,
        runtime.create_failure,
        runtime.result_constructor,
        runtime.resume_with,
        runtime.context_getter,
    ] {
        assert!(frozen.callable(target).is_none());
    }

    // A concrete generated operation owns its dependency fact. Other selected runtime entries
    // remain absent until their own generated operations reference them.
    let read = ir.add_expr(IrExpr::Checked(Box::new(
        IrCheckedOperation::ExternalPropertyRead {
            target: runtime.suspended,
            dispatch: IrPropertyDispatch::Ordinary,
            receiver: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            result: marker.callable.ret,
            substitutions: Vec::new(),
            source_receiver: None,
        },
    )));
    ir.add_fun(IrFunction {
        name: "readMarker".to_owned(),
        params: Vec::new(),
        ret: marker.callable.ret,
        body: Some(read),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let frozen = CheckedBackendCallables::freeze(&ir, provider).unwrap();
    assert_eq!(
        frozen.property(runtime.suspended).unwrap().getter,
        property.getter
    );
    assert_eq!(
        frozen
            .callable(property.getter)
            .unwrap()
            .dependency_callable
            .as_ref(),
        Some(&marker_identity)
    );
    for target in [
        runtime.throw_on_failure,
        runtime.create_failure,
        runtime.result_constructor,
        runtime.resume_with,
        runtime.context_getter,
    ] {
        assert!(frozen.callable(target).is_none());
    }
}

fn assert_provider_identity(
    provider: &KlibLibraries,
    selected: crate::fir::ExternalCallableId,
) -> KlibDependencyCallable {
    let declaration = provider.external_callable(selected).unwrap();
    let identity = declaration
        .dependency_callable
        .expect("an exact KLIB identity");
    let (rebound, declaration) = provider.external_callable_by_dependency(&identity).unwrap();
    assert_eq!(selected, rebound);
    assert_eq!(declaration.dependency_callable.as_ref(), Some(&identity));
    assert!(provider.is_platform_stdlib_dependency(&identity));
    identity
}
