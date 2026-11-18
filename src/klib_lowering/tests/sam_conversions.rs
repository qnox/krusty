//! A genuine Wasm KLIB keeps SAM conversions in its ordinary and prepared bodies.
//! This pins checked common IR, not backend support for dependency SAM realization.

use super::*;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_selected_sam/Library.kt");

/// The externals production registers for the checked closure's target-realized members, such
/// as `String.plus` called inside a converted lambda: each frozen member edge links its exact
/// provider declaration at the receiver instead of lowering the member's library body.
pub(super) fn checked_member_externals(
    libraries: &KlibLibraries,
    checked: &crate::ir::CheckedKlibDependencyFacts,
) -> crate::fx_hash::HashMap<
    crate::libraries::KlibDependencyCallable,
    super::super::EmittedDependencyCallable,
> {
    checked
        .member_edges()
        .keys()
        .map(|identity| {
            let (external, realization) = libraries
                .external_callable_by_dependency(identity)
                .expect("a frozen member edge has an exact provider declaration");
            assert_eq!(realization.dependency_callable.as_ref(), Some(identity));
            (
                identity.clone(),
                super::super::EmittedDependencyCallable::target_member(external),
            )
        })
        .collect()
}

fn assert_sam_frames(ir: &IrFile, method: crate::fir::ExternalCallableId, count: usize) {
    let targets = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                sam: Some(target),
                ..
            } => Some((*impl_fn, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), count);
    for (implementation, target) in targets {
        assert_eq!(
            target.method_target,
            crate::ir::IrSamMethod::External(method)
        );
        assert_eq!(target.parameters, vec![Ty::String]);
        assert_eq!(target.result, Ty::String);
        assert!(!target.wraps_function_value);
        assert!(!target.nullable);
        assert_eq!(
            ir.lambda_sam_signature[&implementation],
            (target.declared_parameters.clone(), target.declared_result)
        );
        assert!(
            matches!(target.declared_parameters.as_slice(), [parameter] if matches!(parameter.kind(), TyKind::TyParam(..)))
        );
        assert_eq!(target.declared_parameters[0], target.declared_result);
    }
    validated(ir);
}

fn assert_sam_result_types(ir: &IrFile, expected: Ty) {
    for (expression, node) in ir.exprs.iter().enumerate() {
        if matches!(node, IrExpr::Lambda { sam: Some(_), .. }) {
            assert_eq!(ir.checked_type(expression as ExprId), Some(expected));
        }
    }
}

fn assert_function_value_sam_frame(ir: &IrFile, method: crate::fir::ExternalCallableId) {
    let targets = ir
        .exprs
        .iter()
        .filter_map(|node| match node {
            IrExpr::Lambda {
                impl_fn,
                captures,
                sam: Some(target),
                ..
            } => Some((*impl_fn, captures, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    // The private initializer remains a literal conversion; the selected factory wraps its
    // function parameter once, with that function's own type retained in its capture.
    assert_eq!(targets.len(), 2);
    let wrapped = targets
        .into_iter()
        .filter(|(_, _, target)| target.wraps_function_value)
        .collect::<Vec<_>>();
    let [(implementation, captures, target)] = wrapped.as_slice() else {
        panic!("one function-value SAM wrapper")
    };
    let function_type = Ty::fun(vec![Ty::String], Ty::String);
    assert_eq!(captures.len(), 1);
    assert!(matches!(ir.expr(captures[0]), IrExpr::GetValue(0)));
    assert_eq!(ir.checked_type(captures[0]), Some(function_type));
    assert_eq!(
        ir.functions[*implementation as usize].params,
        vec![function_type, Ty::String]
    );
    assert_eq!(ir.functions[*implementation as usize].ret, Ty::String);
    assert_eq!(
        target.method_target,
        crate::ir::IrSamMethod::External(method)
    );
    assert_eq!(target.parameters, vec![Ty::String]);
    assert_eq!(target.result, Ty::String);
    assert!(!target.nullable);
    assert_eq!(
        ir.lambda_sam_signature[implementation],
        (target.declared_parameters.clone(), target.declared_result)
    );
    validated(ir);
}

#[test]
fn official_sam_conversions_freeze_and_lower_file_initializer_and_prepared_frames() {
    let target = crate::compilation_target::CompilationTarget::WasmWasi;
    let Some(stdlib) = crate::toolchain::kotlin_stdlib_klib(target) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "selected Wasm stdlib required"
        );
        return;
    };
    let compiler = crate::toolchain::kotlinc_lib_dir()
        .expect("selected producer distribution")
        .with_file_name("bin")
        .join("kotlinc-wasm");
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let work = loop {
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "krusty-klib-selected-sam-{}-{ordinal}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create selected-SAM producer directory: {error}"),
        }
    };
    let source = work.join("Library.kt");
    std::fs::write(&source, LIBRARY).unwrap();
    let library = work.join("sam");
    let output = std::process::Command::new(compiler)
        .args([
            "-Xwasm-target=wasm-wasi",
            "-Xir-produce-klib-dir",
            "-libraries",
        ])
        .arg(&stdlib)
        .arg("-ir-output-dir")
        .arg(&library)
        .args(["-ir-output-name", "selected_sam"])
        .arg(&source)
        .output()
        .expect("run exact Wasm selected-SAM KLIB producer");
    assert!(
        output.status.success(),
        "selected-SAM producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(stdlib, KlibStdlibSemantics::Wasm),
        KlibLibraryInput::dependency(library),
    ])
    .expect("read complete official selected-SAM artifact");
    let classifier = libraries
        .classifier(type_name("fixture/Action"))
        .expect("selected interface");
    let identity = classifier
        .klib_identity
        .as_ref()
        .expect("exact interface identity");
    let method = classifier.declared_callables["apply"].functions()[0]
        .callable
        .external_identity
        .unwrap();
    for (name, inline, wraps_function_value) in [
        ("make", false, false),
        ("inlineMake", true, false),
        ("functionValue", false, true),
    ] {
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), name);
        let [function] = symbols.callables.functions() else {
            panic!("one producer function")
        };
        let selected = libraries
            .external_callable(function.callable.external_identity.unwrap())
            .unwrap();
        let callable = selected.klib_body_callable().unwrap();
        let mut unchecked = DependencyBodyUnit::default();
        let refusal = unchecked
            .lower_function_with_intrinsics(
                callable,
                libraries.declaration_bodies(),
                &KlibCalleeFacts::new([callable]),
                libraries.intrinsic_facts(),
            )
            .expect_err("serialized SAM metadata cannot replace the checked method selection");
        assert_eq!(
            refusal.reason(),
            &KlibBodyDeclineReason::UnsupportedOperation(
                "a SAM conversion without a checked declaration"
            )
        );
        super::demo_package::assert_unit_is_empty(&unchecked);
        let checked = crate::resolve::for_checked_klib_bodies(
            &libraries,
            libraries.declaration_bodies(),
            [callable],
            &[],
        )
        .unwrap();
        let frozen = checked
            .sam_declarations()
            .get(&(
                callable.identity().module.clone(),
                identity.signature.clone(),
            ))
            .expect("file initializer and direct conversion share exact checked declaration");
        assert_eq!(frozen.classifier.klib_identity.as_ref(), Some(identity));
        assert_eq!(
            frozen.target.method_target,
            crate::ir::IrSamMethod::External(method)
        );
        let callees = KlibCalleeFacts::new([callable]).with_checked_dependency_facts(&checked);
        let externals = checked_member_externals(&libraries, &checked);
        let mut raw = DependencyBodyUnit::default();
        raw.lower_function_with_intrinsics_and_externals(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap_or_else(|decline| panic!("raw selected SAM {name}: {decline}"));
        assert_sam_result_types(raw.ir(), callable.ret());
        if wraps_function_value {
            assert_function_value_sam_frame(raw.ir(), method);
        } else {
            // One direct conversion and the otherwise unreferenced private file initializer.
            assert_sam_frames(raw.ir(), method, 2);
        }
        if inline {
            let mut prepared = IrFile::default();
            lower_inline_template(
                &mut prepared,
                &selected,
                libraries.inline_declaration_bodies(),
                TemplateCallees {
                    declarations: libraries.declaration_bodies(),
                    references: libraries.reference_bindings(),
                    source: &libraries,
                    dependency_facts: &checked,
                },
            )
            .unwrap()
            .unwrap_or_else(|decline| panic!("prepared selected SAM: {decline}"));
            assert_sam_result_types(&prepared, callable.ret());
            assert_sam_frames(&prepared, method, 1);
        }
    }
    // One emitted private class needs both header-only inventories: its public interface
    // hierarchy and a SAM in its field initializer, whose body calls no declaration.
    let symbols = libraries.symbols(
        SymbolNamespace::Package(type_name("fixture")),
        "erasedObject",
    );
    let [function] = symbols.callables.functions() else {
        panic!("one erased-return object factory")
    };
    let selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let callable = selected.klib_body_callable().unwrap();
    assert_eq!(callable.ret(), Ty::obj("kotlin/Any"));
    let stored = libraries
        .classifier(type_name("fixture/StoredAction"))
        .unwrap();
    let stored_identity = stored.klib_identity.as_ref().unwrap();
    let stored_method = stored.declared_callables["apply"].functions()[0]
        .callable
        .external_identity
        .unwrap();
    let checked = crate::resolve::for_checked_klib_bodies(
        &libraries,
        libraries.declaration_bodies(),
        [callable],
        &[],
    )
    .unwrap();
    assert_eq!(
        checked.sam_declarations()[&(
            callable.identity().module.clone(),
            stored_identity.signature.clone(),
        )]
            .target
            .method_target,
        crate::ir::IrSamMethod::External(stored_method)
    );
    let hierarchy = checked
        .type_hierarchy()
        .expect("the checked public hierarchy");
    assert!(crate::assignable::TypeOracle::direct_supertypes(
        hierarchy,
        Ty::obj_args("fixture/Leaf", &[Ty::String]),
    )
    .contains(&Ty::obj_args("fixture/Root", &[Ty::String])));
    assert!(libraries
        .classifier(type_name("fixture/HiddenWithSam"))
        .is_none());
    let callees = KlibCalleeFacts::new([callable]).with_checked_dependency_facts(&checked);
    let externals = checked_member_externals(&libraries, &checked);
    let mut raw = DependencyBodyUnit::default();
    // As dependency body emission does, the provider resolves the declarations the private
    // class's members override, which its body never calls.
    raw.lower_function_with_declarations(
        callable,
        libraries.declaration_bodies(),
        &callees,
        libraries.intrinsic_facts(),
        &externals,
        &libraries,
    )
    .unwrap_or_else(|decline| panic!("private class hierarchy and initializer SAM: {decline}"));
    let targets = raw
        .ir()
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, node)| match node {
            IrExpr::Lambda {
                sam: Some(target), ..
            } => Some((id as ExprId, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 2);
    assert_eq!(
        targets
            .iter()
            .filter(|(_, target)| target.method_target
                == crate::ir::IrSamMethod::External(stored_method))
            .count(),
        1
    );
    for (expression, target) in targets {
        let classifier = if target.method_target == crate::ir::IrSamMethod::External(stored_method)
        {
            "fixture/StoredAction"
        } else {
            assert_eq!(
                target.method_target,
                crate::ir::IrSamMethod::External(method)
            );
            "fixture/Action"
        };
        assert_eq!(
            raw.ir().checked_type(expression),
            Some(Ty::obj_args(classifier, &[Ty::String]))
        );
    }
    validated(raw.ir());
    let root = libraries.classifier(type_name("fixture/Root")).unwrap();
    let get = root.declared_callables["get"].functions()[0]
        .callable
        .external_identity
        .unwrap();
    assert!(raw.ir().function_overrides.values().flatten().any(|edge| {
        edge.overridden == crate::fir::ResolvedFunctionOverrideTarget::External(get)
            && edge.overridden_owner == type_name("fixture/Root")
            && edge.applied_result == Ty::String
            && edge.implementation_result == Ty::String
    }));
    std::fs::remove_dir_all(work).unwrap();
}
