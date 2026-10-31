//! Official serialized raw/prepared frames and unsupported nominal closure admission.

use super::*;
use crate::ir::{IrCapturingCallable, IrParameterRole};
use crate::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use crate::libraries::ExternalCallableRealization;

const LIBRARY: &str = include_str!("../../../tests/fixtures/klib_closure_frames/Library.kt");

fn assert_frames(ir: &IrFile, capturer: IrCapturingCallable) {
    let frames = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                arity,
                captures,
                ..
            } => {
                let function = &ir.functions[*impl_fn as usize];
                assert_eq!(ir.lambda_own_params_from[impl_fn], captures.len() as u32);
                assert_eq!(
                    ir.fn_params[impl_fn].identities.len(),
                    function.params.len()
                );
                for identity in &ir.fn_params[impl_fn].identities[..captures.len()] {
                    let IrParameterRole::CapturedValue { capture, .. } = identity.role else {
                        panic!("these closure inputs are captured values")
                    };
                    assert_eq!(capture.capturer, capturer);
                }
                Some((*arity, function.params.clone(), captures.len()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        frames,
        vec![
            (1, vec![Ty::Int, Ty::String, Ty::Int, Ty::Int, Ty::Int], 4),
            (2, vec![Ty::Int, Ty::String, Ty::Int, Ty::Int], 2),
        ]
    );
}

fn producer_function(libraries: &KlibLibraries, name: &str) -> ExternalCallableRealization {
    let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), name);
    let [function] = symbols.callables.functions() else {
        panic!("one producer declaration")
    };
    libraries
        .external_callable(function.callable.external_identity.unwrap())
        .unwrap()
}

#[test]
fn official_serialized_closure_frames_keep_raw_and_prepared_ownership() {
    let Some(root) = distribution_root() else {
        return;
    };
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let work = loop {
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "krusty-klib-closure-frames-{}-{ordinal}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => break directory,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create producer directory: {error}"),
        }
    };
    let source = work.join("Frames.kt");
    std::fs::write(&source, LIBRARY).unwrap();
    let library = work.join("frames");
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
    .expect("read official serialized closure declarations");
    for (name, capturer) in [
        ("make", IrCapturingCallable::Lambda),
        ("inlineMake", IrCapturingCallable::Lambda),
        ("anonymousMake", IrCapturingCallable::AnonymousFunction),
    ] {
        let selected = producer_function(&libraries, name);
        let callable = selected.klib_body_callable().unwrap();
        let callees = KlibCalleeFacts::new([callable]);
        let mut raw = DependencyBodyUnit::default();
        raw.lower_function_with_intrinsics(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
        )
        .unwrap_or_else(|decline| panic!("raw {name}: {decline}"));
        validated(raw.ir());
        assert_frames(raw.ir(), capturer);
        if name == "inlineMake" {
            let mut prepared = IrFile::default();
            lower_inline_template(
                &mut prepared,
                &selected,
                libraries.inline_declaration_bodies(),
                TemplateCallees {
                    declarations: libraries.declaration_bodies(),
                    references: libraries.reference_bindings(),
                    source: &libraries,
                },
            )
            .unwrap()
            .unwrap_or_else(|decline| panic!("prepared: {decline}"));
            validated(&prepared);
            assert_frames(&prepared, capturer);
        }
    }
    // This is valid producer IR with a nominal SAM result and Comparator.compare override.
    // The first closure slice must decline it, not diagnose an internally inconsistent FunctionN.
    let selected = producer_function(&libraries, "unsupportedComparator");
    let callable = selected.klib_body_callable().unwrap();
    let callees = KlibCalleeFacts::new([callable]);
    let mut raw = DependencyBodyUnit::default();
    let decline = raw
        .lower_function_with_intrinsics(
            callable,
            libraries.declaration_bodies(),
            &callees,
            libraries.intrinsic_facts(),
        )
        .expect_err("SAM references remain a separate capability");
    assert_eq!(
        decline.reason(),
        &KlibBodyDeclineReason::UnsupportedOperation("a closure of a non-function type")
    );
    assert!(!decline.is_internal_inconsistency());
    super::demo_package::assert_unit_is_empty(&raw);
    std::fs::remove_dir_all(work).unwrap();
}
