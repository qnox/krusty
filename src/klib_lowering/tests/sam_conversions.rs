//! A genuine Wasm KLIB keeps SAM conversions in its ordinary and prepared bodies.
//! This pins checked common IR, not backend support for dependency SAM realization.

use super::*;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_selected_sam/Library.kt");
/// The same conversions with bodies that call nothing, so the test needs no declaration facts
/// beyond the SAM selection.
const NATIVE_LIBRARY: &str = include_str!("../../../tests/fixtures/klib_native_sam/Library.kt");
const NATIVE_OTHER: &str = include_str!("../../../tests/fixtures/klib_native_sam/Other.kt");

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

pub(super) fn wasm_library(
    library_source: &str,
    module: &str,
) -> Option<(std::path::PathBuf, KlibLibraries)> {
    let target = crate::compilation_target::CompilationTarget::WasmWasi;
    let Some(stdlib) = crate::toolchain::kotlin_stdlib_klib(target) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "selected Wasm stdlib required"
        );
        return None;
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
    std::fs::write(&source, library_source).unwrap();
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
        .args(["-ir-output-name", module])
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
        KlibLibraryInput::dependency(library.clone()),
    ])
    .expect("read complete official selected-SAM artifact");
    Some((work, libraries))
}

#[test]
fn official_sam_conversions_freeze_and_lower_file_initializer_and_prepared_frames() {
    let Some((work, libraries)) = wasm_library(LIBRARY, "selected_sam") else {
        return;
    };
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
    for (name, inline) in [("make", false), ("inlineMake", true)] {
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
        // One direct conversion and the otherwise unreferenced private file initializer.
        assert_sam_frames(raw.ir(), method, 2);
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
            assert_sam_frames(&prepared, method, 1);
        }
    }
    std::fs::remove_dir_all(work).unwrap();
}

/// Native serializes the same conversions as lambdas whose rich reference has the interface's
/// type instead of a `SAM_CONVERSION` of a function value. They freeze and attach the same
/// checked declaration.
#[test]
fn native_converted_lambdas_freeze_and_lower_the_same_checked_declaration() {
    let Some(root) = distribution_root() else {
        return;
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let work = loop {
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "krusty-klib-native-sam-{}-{ordinal}",
            std::process::id()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create Native SAM producer directory: {error}"),
        }
    };
    let source = work.join("Library.kt");
    std::fs::write(&source, NATIVE_LIBRARY).unwrap();
    let other_source = work.join("Other.kt");
    std::fs::write(&other_source, NATIVE_OTHER).unwrap();
    let library = work.join("sam");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .arg(&other_source)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library.clone()),
    ])
    .expect("read official Native SAM declarations");
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
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "make");
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
        .expect_err("a converted lambda's type cannot replace the checked method selection");
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
        .expect("the converted lambdas freeze the exact checked declaration");
    assert_eq!(frozen.classifier.klib_identity.as_ref(), Some(identity));
    let callees = KlibCalleeFacts::new([callable]).with_checked_dependency_facts(&checked);
    let mut raw = DependencyBodyUnit::default();
    raw.lower_function_with_intrinsics(
        callable,
        libraries.declaration_bodies(),
        &callees,
        libraries.intrinsic_facts(),
    )
    .unwrap_or_else(|decline| panic!("Native converted lambda: {decline}"));
    // One direct conversion and the otherwise unreferenced private file initializer.
    assert_sam_frames(raw.ir(), method, 2);
    let interface = Ty::obj_args("fixture/Action", &[Ty::String]);
    for (id, expression) in raw.ir().exprs.iter().enumerate() {
        if matches!(expression, IrExpr::Lambda { sam: Some(_), .. }) {
            assert_eq!(raw.ir().logical_type(id as ExprId), Some(interface));
        }
    }

    // `Other.apply` has the name and shape of `Action.apply`. The override a converted lambda
    // names is authenticated against the frozen declaration's exact method identities, so the
    // unrelated owner's method never stands for the selected one.
    let other_classifier = libraries
        .classifier(type_name("fixture/Other"))
        .expect("unrelated interface");
    let other_identity = other_classifier
        .klib_identity
        .as_ref()
        .expect("exact unrelated interface identity");
    let other_method = other_classifier.declared_callables["apply"].functions()[0]
        .callable
        .external_identity
        .unwrap();
    let dependency = |id| {
        libraries
            .external_callable(id)
            .unwrap()
            .dependency_callable
            .expect("exact serialized method identity")
    };
    let (action_apply, other_apply) = (dependency(method), dependency(other_method));
    assert_ne!(action_apply, other_apply);
    assert_eq!(
        frozen.overridable.as_ref(),
        std::slice::from_ref(&action_apply)
    );
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "other");
    let [function] = symbols.callables.functions() else {
        panic!("one unrelated producer function")
    };
    let other_selected = libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap();
    let other_callable = other_selected.klib_body_callable().unwrap();
    let other_checked = crate::resolve::for_checked_klib_bodies(
        &libraries,
        libraries.declaration_bodies(),
        [other_callable],
        &[],
    )
    .unwrap();
    let other_frozen = other_checked
        .sam_declarations()
        .get(&(
            other_callable.identity().module.clone(),
            other_identity.signature.clone(),
        ))
        .expect("the unrelated lambda freezes its own interface's declaration");
    assert_eq!(
        other_frozen.overridable.as_ref(),
        std::slice::from_ref(&other_apply)
    );
    let other_callees =
        KlibCalleeFacts::new([other_callable]).with_checked_dependency_facts(&other_checked);
    let mut other_raw = DependencyBodyUnit::default();
    other_raw
        .lower_function_with_intrinsics(
            other_callable,
            libraries.declaration_bodies(),
            &other_callees,
            libraries.intrinsic_facts(),
        )
        .unwrap_or_else(|decline| panic!("unrelated converted lambda: {decline}"));
    assert_sam_frames(other_raw.ir(), other_method, 1);
    // Facts frozen for `Action` alone give the unrelated lambda no declaration to borrow.
    let action_only =
        KlibCalleeFacts::new([other_callable]).with_checked_dependency_facts(&checked);
    let mut relabeled = DependencyBodyUnit::default();
    let refusal = relabeled
        .lower_function_with_intrinsics(
            other_callable,
            libraries.declaration_bodies(),
            &action_only,
            libraries.intrinsic_facts(),
        )
        .expect_err("another interface's frozen declaration never selects this lambda's method");
    assert_eq!(
        refusal.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation(
            "a SAM conversion without a checked declaration"
        )
    );
    super::demo_package::assert_unit_is_empty(&relabeled);

    // The producer's own `make` body with one serialized fact rewritten: its `Action` lambda
    // names the unrelated `Other.apply` as the method it overrides. The `Action` target and its
    // checked facts stay valid, so only the overridden-symbol comparison can refuse it.
    let mut trees = crate::metadata::klib_ir::read_declaration_trees(
        &crate::klib::KlibArchive::open(&library).expect("the producer library opens"),
    )
    .expect("the producer IR decodes")
    .into_trees();
    let converted = |tree: &crate::metadata::klib_ir::KlibIrDeclarationTree, name: &str| {
        let KlibIrMember::Function(function) = tree.declaration else {
            return None;
        };
        if tree.arena.function(function).name != name {
            return None;
        }
        (0..tree.arena.expr_count())
            .map(|index| tree.arena.expr_id(index))
            .find(|&id| {
                matches!(
                    tree.arena.expr(id).kind,
                    KlibIrExprKind::RichFunctionReference { .. }
                )
            })
    };
    let other_override = trees
        .iter()
        .find_map(|tree| {
            let id = converted(tree, "other")?;
            let KlibIrExprKind::RichFunctionReference { overridden, .. } =
                &tree.arena.expr(id).kind
            else {
                unreachable!("the reference was found by its kind")
            };
            Some(overridden.clone())
        })
        .expect("the unrelated lambda names `Other.apply`");
    assert_eq!(
        other_override.signature,
        KlibIrSignature::Public(match &other_apply.declaration {
            KlibDeclarationSignature::Public(signature) => signature.clone(),
            other => panic!("a public method: {other:?}"),
        })
    );
    let (index, reference) = trees
        .iter()
        .enumerate()
        .find_map(|(index, tree)| Some((index, converted(tree, "make")?)))
        .expect("`make` converts one lambda");
    let KlibIrExprKind::RichFunctionReference { overridden, .. } =
        &mut trees[index].arena.expr_mut(reference).kind
    else {
        unreachable!("the reference was found by its kind")
    };
    *overridden = other_override;
    let rewritten = KlibDeclarationBodies::from_libraries(vec![(
        callable.identity().module.clone(),
        crate::metadata::klib_ir::KlibIrModuleTrees::from_trees(trees)
            .expect("the rewritten trees index"),
    )])
    .expect("the rewritten module joins");
    let mut wrong = DependencyBodyUnit::default();
    let refusal = wrong
        .lower_function_with_intrinsics(callable, &rewritten, &callees, libraries.intrinsic_facts())
        .expect_err("`Other.apply` never stands for the checked `Action.apply`");
    assert_eq!(
        refusal.reason(),
        &KlibBodyDeclineReason::SignatureMismatch(
            "a converted lambda overrides another method than its checked SAM method".to_owned()
        )
    );
    super::demo_package::assert_unit_is_empty(&wrong);
    std::fs::remove_dir_all(work).unwrap();
}
