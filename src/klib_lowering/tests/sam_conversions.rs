//! A genuine Wasm KLIB keeps SAM conversions in its ordinary and prepared bodies.
//! This pins checked common IR, not backend support for dependency SAM realization.

use super::*;
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_selected_sam/Library.kt");
/// The same conversions with bodies that call nothing, so the test needs no declaration facts
/// beyond the SAM selection.
const NATIVE_LIBRARY: &str = include_str!("../../../tests/fixtures/klib_native_sam/Library.kt");

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
        KlibLibraryInput::dependency(library),
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
        let mut raw = DependencyBodyUnit::default();
        raw.lower_function_with_intrinsics(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
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
    let library = work.join("sam");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
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
        KlibLibraryInput::dependency(library),
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
    std::fs::remove_dir_all(work).unwrap();
}
