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
    let runtime = select(provider).expect("one exact coroutine runtime");
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

    let get_or_throw = provider.external_callable(runtime.get_or_throw).unwrap();
    let get_or_throw_identity = assert_provider_identity(provider, runtime.get_or_throw);
    assert_eq!(get_or_throw.kind, ExternalCallableKind::Extension);
    assert_eq!(get_or_throw.callable.name, "getOrThrow");
    assert_eq!(
        get_or_throw
            .callable
            .source_receiver
            .map(Ty::non_null)
            .and_then(Ty::obj_internal),
        Some(type_name("kotlin/Result"))
    );

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

    let failure = provider.external_callable(runtime.result_failure).unwrap();
    let failure_identity = assert_provider_identity(provider, runtime.result_failure);
    assert_eq!(failure.kind, ExternalCallableKind::Member);
    assert_eq!(failure.callable.name, "failure");
    let companion = provider
        .classifier(type_name("kotlin/Result"))
        .unwrap()
        .companion_object
        .as_ref()
        .unwrap()
        .1;
    assert_eq!(
        failure.declaration_owner,
        Some(SemanticCallableOwner::Classifier(companion))
    );
    assert!(failure.callable.source_receiver.is_none());
    assert_eq!(failure.callable.params.len(), 1);
    assert_eq!(
        failure.callable.params[0].non_null().obj_internal(),
        Some(type_name("kotlin/Throwable"))
    );
    assert_eq!(
        failure.callable.ret.non_null().obj_internal(),
        Some(type_name("kotlin/Result"))
    );
    let success = provider.external_callable(runtime.result_success).unwrap();
    assert_provider_identity(provider, runtime.result_success);
    assert_eq!(success.kind, ExternalCallableKind::Member);
    assert_eq!(success.callable.name, "success");
    assert_eq!(
        success.callable.declaration_owner,
        Some(SemanticCallableOwner::Classifier(companion))
    );
    assert_ne!(get_or_throw_identity, failure_identity);
    assert_ne!(resume_with_identity, failure_identity);

    assert_only_generated_uses_are_frozen(provider, runtime);
}

fn assert_only_generated_uses_are_frozen(provider: &KlibLibraries, runtime: IrCoroutineRuntime) {
    let property = provider.external_property(runtime.suspended).unwrap();
    let marker = provider.external_callable(property.getter).unwrap();
    let marker_identity = assert_provider_identity(provider, property.getter);

    // Selection supplies declaration evidence. It does not make a marker read, completion call,
    // or failure delivery executable, and must not add any of them to the frozen IR inventory.
    let mut ir = IrFile {
        coroutine_runtime: Some(runtime),
        ..IrFile::default()
    };
    let frozen = CheckedBackendCallables::freeze(&ir, provider).unwrap();
    assert!(frozen.property(runtime.suspended).is_none());
    assert!(frozen.property(runtime.context).is_none());
    for target in [
        runtime.get_or_throw,
        runtime.resume_with,
        runtime.context_getter,
        runtime.result_success,
        runtime.result_failure,
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
        runtime.get_or_throw,
        runtime.resume_with,
        runtime.context_getter,
        runtime.result_success,
        runtime.result_failure,
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
