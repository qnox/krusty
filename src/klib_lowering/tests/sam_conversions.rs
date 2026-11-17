//! A genuine Wasm KLIB keeps SAM conversions in its ordinary and prepared bodies.
//! This pins checked common IR, not backend support for dependency SAM realization.

use super::*;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_selected_sam/Library.kt");
/// The same conversions with bodies that call nothing, so the test needs no declaration facts
/// beyond the SAM selection.
const NATIVE_LIBRARY: &str = include_str!("../../../tests/fixtures/klib_native_sam/Library.kt");
const NATIVE_OTHER: &str = include_str!("../../../tests/fixtures/klib_native_sam/Other.kt");

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

/// Resolve only the exact member identities the checker already authorized, preserving the
/// declaration snapshots while the callee facts borrow their headers, as body emission does.
pub(super) fn checked_member_declarations(
    libraries: &KlibLibraries,
    checked: &crate::ir::CheckedKlibDependencyFacts,
) -> Vec<(
    crate::fir::ExternalCallableId,
    crate::libraries::ExternalCallableRealization,
)> {
    checked
        .member_edges()
        .keys()
        .map(|identity| {
            let (target, declaration) = libraries
                .external_callable_by_dependency(identity)
                .expect("a frozen member edge retains its exact provider declaration");
            assert_eq!(declaration.dependency_callable.as_ref(), Some(identity));
            (target, declaration)
        })
        .collect()
}

// A narrow common-IR test target: Wasm realizes primitive operators directly and owns String's
// representation. These are target capabilities, not selections of producer/member spellings.
pub(super) fn supports_sam_ir_operation(operation: crate::libraries::CompilerIntrinsic) -> bool {
    crate::ir::IrBinOp::of_primitive_operator(operation).is_some()
}

pub(super) fn represents_sam_ir_classifier(classifier: crate::types::TypeName) -> bool {
    classifier == crate::types::wk::string()
}

/// The same authenticated leaf classifications used by production dependency-body planning.
/// A selected header alone never makes a call a target leaf; the target capability and, for a
/// represented member, the checker's actual call edge must authorize that realization.
pub(super) fn checked_member_leaves(
    libraries: &KlibLibraries,
    callees: &KlibCalleeFacts<'_>,
    declarations: &[(
        crate::fir::ExternalCallableId,
        crate::libraries::ExternalCallableRealization,
    )],
    supports: &dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
    represents: &dyn Fn(crate::types::TypeName) -> bool,
) -> crate::fx_hash::HashMap<
    crate::libraries::KlibDependencyCallable,
    super::super::EmittedDependencyCallable,
> {
    declarations
        .iter()
        .filter_map(|(target, declaration)| {
            let identity = declaration.dependency_callable.as_ref().unwrap();
            let leaf = if let Some(operation) = callees
                .target_intrinsic(identity, libraries.declaration_bodies(), supports)
                .expect("a target leaf authenticates its exact producer header")
            {
                super::super::EmittedDependencyCallable::target_realization(*target, operation)
            } else if callees
                .target_member(identity, represents)
                .expect("a target member has one checked declaration")
                && callees.member_edge(identity).is_some()
            {
                super::super::EmittedDependencyCallable::target_member(*target)
            } else {
                return None;
            };
            Some((identity.clone(), leaf))
        })
        .collect()
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
        let member_snapshots = checked_member_declarations(&libraries, &checked);
        let callees = KlibCalleeFacts::with_reference_declarations(
            [callable],
            member_snapshots.iter().filter_map(|(target, declaration)| {
                declaration
                    .klib_body_callable()
                    .map(|callable| (*target, callable))
            }),
        )
        .with_checked_dependency_facts(&checked);
        let externals = checked_member_leaves(
            &libraries,
            &callees,
            &member_snapshots,
            &supports_sam_ir_operation,
            &represents_sam_ir_classifier,
        );
        assert!(checked_member_leaves(
            &libraries,
            &callees,
            &member_snapshots,
            &|_| false,
            &|_| false,
        )
        .is_empty());
        let represented_members = checked_member_leaves(
            &libraries,
            &callees,
            &member_snapshots,
            &|_| false,
            &represents_sam_ir_classifier,
        );
        assert!(!represented_members.is_empty());
        assert_eq!(represented_members.len(), externals.len());
        let mut raw = DependencyBodyUnit::default();
        raw.lower_function_with_declarations(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
            &externals,
            &libraries,
        )
        .unwrap_or_else(|decline| panic!("raw selected SAM {name}: {decline}"));
        let retained =
            crate::backend::CheckedBackendCallables::freeze(raw.ir(), &libraries).unwrap();
        for (identity, member) in &represented_members {
            assert_eq!(
                retained
                    .callable(member.target())
                    .unwrap()
                    .dependency_callable
                    .as_ref(),
                Some(identity)
            );
            assert!(checked.member_edges().contains_key(identity));
        }
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
    let member_snapshots = checked_member_declarations(&libraries, &checked);
    let callees = KlibCalleeFacts::with_reference_declarations(
        [callable],
        member_snapshots.iter().filter_map(|(target, declaration)| {
            declaration
                .klib_body_callable()
                .map(|callable| (*target, callable))
        }),
    )
    .with_checked_dependency_facts(&checked);
    let externals = checked_member_leaves(
        &libraries,
        &callees,
        &member_snapshots,
        &supports_sam_ir_operation,
        &represents_sam_ir_classifier,
    );
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
