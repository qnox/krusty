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

#[test]
fn official_receiver_capture_reads_match_checked_source_in_raw_and_prepared_bodies() {
    // Requires the official Native KLIB producer; the KLIB lane requires its exact toolchain.
    let Some(root) = distribution_root() else {
        return;
    };
    let source = include_str!("../../../tests/fixtures/klib_closure_frames/ReceiverCapture.kt");
    let checked = crate::fir_lower::tests::lower_single_source_with_platform(
        source,
        "ReceiverCapture",
        Box::new(crate::libraries::EmptySymbolSource),
    );
    // Compare the complete lifted body, its physical parameter order and its semantic capture
    // identities. Generated function ids and implementation names are not declaration identity.
    let closure_contract = |ir: &IrFile| {
        ir.exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::Lambda { impl_fn, arity, .. } => {
                    let function = &ir.functions[*impl_fn as usize];
                    let parameters = &ir.fn_params[impl_fn];
                    Some((
                        *arity,
                        function.params.clone(),
                        function.ret,
                        parameters.identities.clone(),
                        parameters.captured_receivers.clone(),
                        render(ir, function.body.expect("a lifted closure has a body")),
                    ))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let expected = closure_contract(&checked);
    assert_eq!(
        expected.len(),
        1,
        "the source captures a value and its receiver"
    );
    let work = std::env::temp_dir().join(format!(
        "krusty-klib-receiver-capture-{}",
        std::process::id()
    ));
    std::fs::create_dir(&work).expect("create receiver capture producer directory");
    let input = work.join("ReceiverCapture.kt");
    std::fs::write(&input, source).unwrap();
    let library = work.join("receiver-capture");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&input)
        .output()
        .expect("run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .expect("read official receiver capture declaration");
    let selected = producer_function(&libraries, "capture");
    let callable = selected.klib_body_callable().unwrap();
    let mut raw = DependencyBodyUnit::default();
    raw.lower_function_with_intrinsics(
        callable,
        libraries.declaration_bodies(),
        &KlibCalleeFacts::new([callable]),
        libraries.intrinsic_facts(),
    )
    .expect("lower raw receiver capture");
    validated(raw.ir());
    assert_eq!(closure_contract(raw.ir()), expected, "raw/source closure");
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
    .expect("lower prepared receiver capture");
    validated(&prepared);
    assert_eq!(
        closure_contract(&prepared),
        expected,
        "prepared/source closure"
    );
    std::fs::remove_dir_all(work).unwrap();
}
