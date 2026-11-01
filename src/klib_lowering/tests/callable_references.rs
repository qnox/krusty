//! Official declaration-reference identities, nominal types and transactional refusal.

use super::*;
use crate::fir::ExternalCallableId;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use crate::libraries::ExternalCallableRealization;

struct ProducerDirectory(PathBuf);

impl ProducerDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "krusty-klib-reference-{}-{ordinal}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create reference producer directory: {error}"),
            }
        }
    }
}

impl Drop for ProducerDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn selected(
    libraries: &KlibLibraries,
    name: &str,
) -> (ExternalCallableId, ExternalCallableRealization) {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), name);
    let [function] = symbols.callables.functions() else {
        panic!("one producer function")
    };
    let target = function.callable.external_identity.unwrap();
    (target, libraries.external_callable(target).unwrap())
}

fn assert_reference(ir: &IrFile, target: ExternalCallableId) {
    validated(ir);
    let references = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expression)| {
            if let IrExpr::CallableReference(reference) = expression {
                Some((id as u32, reference))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 1);
    let (expression, reference) = references[0];
    assert!(matches!(reference.target,
        crate::ir::IrCallableReferenceTarget::External { declaration, receiver: None }
            if declaration == target));
    assert_eq!(reference.captures, Vec::<ExprId>::new());
    assert_eq!(reference.bound_receiver, None);
    assert_eq!(reference.function_type, Ty::fun(vec![Ty::Int], Ty::Int));
    assert_eq!(reference.declaration_parameters.as_ref(), &[Ty::Int]);
    assert_eq!(reference.declaration_result, Ty::Int);
    assert!(!reference.declaration_suspend);
    assert!(reference.adaptation.is_none());
    assert_eq!(reference.reflection_owner, None);
    assert_eq!(
        ir.checked_type(expression),
        Some(Ty::obj_args(
            "kotlin/reflect/KFunction1",
            &[Ty::Int, Ty::Int]
        ))
    );
    let adapter = &ir.functions[reference.adapter as usize];
    assert_eq!(adapter.params, vec![Ty::Int]);
    assert_eq!(adapter.ret, Ty::Int);
    assert_eq!(ir.lambda_own_params_from[&reference.adapter], 0);
    assert_eq!(ir.fn_params[&reference.adapter].identities.len(), 1);
    let body = adapter.body.unwrap();
    let result = ir.callable_reference_adapter_results[&body];
    assert_eq!(ir.checked_type(result), Some(Ty::Int));
    assert!(ir.callable_scopes.contains(&body));
}

fn reference_declarations(
    libraries: &KlibLibraries,
) -> Vec<(ExternalCallableId, ExternalCallableRealization)> {
    let mut pending = ["makeReference", "inlineMakeReference", "makeOtherReference"]
        .into_iter()
        .map(|name| selected(libraries, name).1.dependency_callable.unwrap())
        .collect::<Vec<_>>();
    let mut visited = crate::fx_hash::HashSet::default();
    let mut declarations = Vec::new();
    while let Some(identity) = pending.pop() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        pending.extend(
            libraries
                .declaration_bodies()
                .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS),
        );
        if let Some(declaration) = libraries.external_callable_by_dependency(&identity) {
            declarations.push(declaration);
        } else {
            assert!(DependencyBodyUnit::default()
                .folds_common_builtin(&identity, &KlibCalleeFacts::default()));
        }
    }
    declarations
}

/// Match Native's existing authenticated bodyless/intrinsic leaf boundary. Ordinary referenced
/// functions remain local bodies, so declaration identity does not grant them external authority.
fn reference_externals(
    libraries: &KlibLibraries,
    declarations: &[(ExternalCallableId, ExternalCallableRealization)],
) -> crate::fx_hash::HashMap<KlibDependencyCallable, crate::klib_lowering::EmittedDependencyCallable>
{
    declarations
        .iter()
        .filter_map(|(target, declaration)| {
            let identity = declaration.dependency_callable.as_ref()?;
            (libraries.declaration_bodies().function_has_body(identity) == Some(false)
                || libraries.intrinsic_facts().get(identity).is_some())
            .then(|| {
                (
                    identity.clone(),
                    crate::klib_lowering::EmittedDependencyCallable::new(*target, None, None),
                )
            })
        })
        .collect()
}

#[test]
fn official_references_preserve_raw_prepared_identity_and_refusal_rollback() {
    let Some(root) = distribution_root() else {
        return;
    };
    let work = ProducerDirectory::new();
    let positive = work.0.join("References.kt");
    let negative = work.0.join("Declines.kt");
    std::fs::write(
        &positive,
        include_str!("../../../tests/fixtures/klib_closure_frames/TopLevelReferences.kt"),
    )
    .unwrap();
    std::fs::write(
        &negative,
        include_str!("../../../tests/fixtures/klib_closure_frames/ReferenceDeclines.kt"),
    )
    .unwrap();
    let library = work.0.join("references");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .args([positive, negative])
        .output()
        .expect("run exact Native producer");
    assert!(
        output.status.success(),
        "producer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .unwrap();
    let declarations = reference_declarations(&libraries);
    assert_raw_and_prepared(&libraries, &declarations);
    assert_refusals_and_reuse(&libraries, &declarations);
    assert_ambiguous_identity(&libraries);
}

fn assert_raw_and_prepared(
    libraries: &KlibLibraries,
    declarations: &[(ExternalCallableId, ExternalCallableRealization)],
) {
    let externals = reference_externals(libraries, declarations);
    let step = selected(libraries, "referenceStep");
    let other = selected(libraries, "otherReferenceStep");
    for (name, expected) in [
        ("makeReference", step.0),
        ("inlineMakeReference", step.0),
        ("makeOtherReference", other.0),
    ] {
        let (_, declaration) = selected(libraries, name);
        let callable = declaration.klib_body_callable().unwrap();
        let facts = KlibCalleeFacts::with_reference_declarations(
            [callable],
            declarations.iter().filter_map(|(target, declaration)| {
                declaration
                    .klib_body_callable()
                    .map(|callable| (*target, callable))
            }),
        );
        assert!(!facts.is_selected(step.1.klib_body_callable().unwrap().identity()));
        let mut unit = DependencyBodyUnit::default();
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            libraries.declaration_bodies(),
            &facts,
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap();
        assert_reference(unit.ir(), expected);
        if name == "inlineMakeReference" {
            let mut prepared = IrFile::default();
            lower_inline_template(
                &mut prepared,
                &declaration,
                libraries.inline_declaration_bodies(),
                TemplateCallees {
                    declarations: libraries.declaration_bodies(),
                    references: libraries.reference_bindings(),
                    source: libraries,
                },
            )
            .unwrap()
            .unwrap();
            assert_reference(&prepared, expected);
        }
    }
}

fn assert_refusals_and_reuse(
    libraries: &KlibLibraries,
    declarations: &[(ExternalCallableId, ExternalCallableRealization)],
) {
    let externals = reference_externals(libraries, declarations);
    let step = selected(libraries, "referenceStep");
    for (name, reason) in [
        (
            "boundReference",
            "a bound or reflective rich function reference",
        ),
        (
            "specializedReference",
            "a bound or reflective rich function reference",
        ),
        (
            "unitAdaptedReference",
            "a bound or reflective rich function reference",
        ),
        ("referenceThenLocalClass", "a local class"),
    ] {
        let (_, declaration) = selected(libraries, name);
        let callable = declaration.klib_body_callable().unwrap();
        let facts = KlibCalleeFacts::with_reference_declarations(
            [callable],
            declarations.iter().filter_map(|(target, declaration)| {
                declaration
                    .klib_body_callable()
                    .map(|callable| (*target, callable))
            }),
        );
        let mut unit = DependencyBodyUnit::default();
        let decline = unit
            .lower_function_with_intrinsics_and_externals(
                callable,
                libraries.declaration_bodies(),
                &facts,
                libraries.intrinsic_facts(),
                &externals,
            )
            .unwrap_err();
        assert_eq!(
            decline.reason(),
            &KlibBodyDeclineReason::UnsupportedOperation(reason)
        );
        super::demo_package::assert_unit_is_empty(&unit);
        assert!(unit.ir().callable_reference_adapter_results.is_empty());
        // The next transaction reuses discarded IDs without inheriting an adapter result.
        let (_, next) = selected(libraries, "makeReference");
        let callable = next.klib_body_callable().unwrap();
        let facts = KlibCalleeFacts::with_reference_declarations(
            [callable],
            declarations.iter().filter_map(|(target, declaration)| {
                declaration
                    .klib_body_callable()
                    .map(|callable| (*target, callable))
            }),
        );
        unit.lower_function_with_intrinsics_and_externals(
            callable,
            libraries.declaration_bodies(),
            &facts,
            libraries.intrinsic_facts(),
            &externals,
        )
        .unwrap();
        assert_reference(unit.ir(), step.0);
    }
}

fn assert_ambiguous_identity(libraries: &KlibLibraries) {
    let step = selected(libraries, "referenceStep");
    let other = selected(libraries, "otherReferenceStep");
    let (_, declaration) = selected(libraries, "makeReference");
    let callable = declaration.klib_body_callable().unwrap();
    let step_body = step.1.klib_body_callable().unwrap();
    let facts = KlibCalleeFacts::with_reference_declarations(
        [callable],
        [(step.0, step_body), (other.0, step_body)],
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function_with_intrinsics(
            callable,
            libraries.declaration_bodies(),
            &facts,
            libraries.intrinsic_facts(),
        )
        .unwrap_err();
    assert_eq!(
        decline.reason(),
        &KlibBodyDeclineReason::SignatureMismatch(
            "a callable reference has no unique provider declaration".to_owned()
        )
    );
    super::demo_package::assert_unit_is_empty(&unit);
}
