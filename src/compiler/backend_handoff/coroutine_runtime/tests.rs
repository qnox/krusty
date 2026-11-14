use super::*;
use crate::backend::CheckedBackendCallables;
use crate::compilation_target::CompilationTarget;
use crate::ir::{
    Callee, IrCheckedOperation, IrConst, IrExpr, IrFile, IrFunction, IrPropertyDispatch, IrTypeOp,
};
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
    assert_eq!(runtime.result_companion, companion);
    assert_eq!(
        failure.callable.declaration_owner,
        Some(SemanticCallableOwner::Classifier(runtime.result_companion))
    );
    assert_ne!(get_or_throw_identity, failure_identity);
    assert_ne!(resume_with_identity, failure_identity);

    assert_only_generated_uses_are_frozen(provider, runtime);
    assert_relocated_companion_call_freezes(provider, runtime);
}

fn assert_relocated_companion_call_freezes(provider: &KlibLibraries, runtime: IrCoroutineRuntime) {
    let mut ir = IrFile::default();
    let argument = ir.add_expr(IrExpr::Const(IrConst::String("value".to_owned().into())));
    let result_ty = Ty::obj_args_name(type_name("kotlin/Result"), &[Ty::String]);
    let source = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target: runtime.result_success,
            default_provider: None,
            params: vec![Ty::String],
            ret: result_ty,
            substitutions: vec![crate::ir::IrCheckedSubstitution {
                parameter: crate::fir::FirTypeParameterRef::External {
                    callable: runtime.result_success,
                    ordinal: 0,
                },
                reified: false,
                value: Ty::String,
                reified_runtime: Ty::String,
                additional_bounds: Vec::new(),
            }],
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: None,
        args: vec![argument],
    });
    let companion = ir.add_expr(IrExpr::SingletonValue {
        classifier: runtime.result_companion,
    });
    ir.set_logical_type(companion, Ty::obj_name(runtime.result_companion));
    if let IrExpr::Call {
        dispatch_receiver, ..
    } = &mut ir.exprs[source as usize]
    {
        *dispatch_receiver = Some(companion);
    }
    ir.external_singleton_dispatches
        .insert(source, runtime.result_companion);
    ir.call_declared_ret.insert(source, result_ty);
    ir.set_logical_type(source, result_ty);

    let replacement = crate::ir::relocate_with_operands(&mut ir, source, |operand| operand);
    let discarded_value = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.exprs[source as usize] = IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: discarded_value,
        type_operand: result_ty,
    };
    ir.set_logical_type(source, result_ty);
    let function = ir.add_fun(IrFunction {
        name: "useRelocatedSuccess".to_owned(),
        params: Vec::new(),
        ret: result_ty,
        body: Some(replacement),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    assert_eq!(
        ir.external_singleton_dispatches.get(&replacement),
        Some(&runtime.result_companion)
    );
    assert!(!ir.external_singleton_dispatches.contains_key(&source));

    let frozen = CheckedBackendCallables::freeze(&ir, provider)
        .expect("relocating an operation retires singleton dispatch from its old slot");
    assert!(frozen.callable(runtime.result_success).is_some());
    assert!(ir.functions[function as usize].body.is_some());
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
