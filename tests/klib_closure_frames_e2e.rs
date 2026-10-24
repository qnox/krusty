//! PLATFORM: serialized KLIB closure frames, prepared-inline copies and the Native closure ABI.

use crate::common;
use krusty::diag::DiagSink;
use krusty::ir::{IrCapturingCallable, IrExpr, IrFile, IrParameterRole};
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use krusty::klib_lowering::{DependencyBodyUnit, KlibCalleeFacts, TemplateCallees};
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::{type_name, Ty};

const LIBRARY: &str = r#"package fixture
fun make(seed: Int, prefix: String): (Int, Int) -> String {
    val saved = seed
    return { a, b ->
        val nested = { seed: Int -> "$saved:$prefix:$a:$b:$seed" }
        nested(9)
    }
}
inline fun inlineMake(seed: Int, prefix: String): (Int, Int) -> String {
    val saved = seed
    return { a, b ->
        val nested = { seed: Int -> "$saved:$prefix:$a:$b:$seed" }
        nested(9)
    }
}
fun anonymousMake(seed: Int, prefix: String): (Int, Int) -> String {
    val saved = seed
    return fun(a: Int, b: Int): String {
        val nested = fun(seed: Int): String { return "$saved:$prefix:$a:$b:$seed" }
        return nested(9)
    }
}
"#;

const CONSUMER: &str = r#"
var effects = 0
fun captureNumber(): Int { effects = effects * 10 + 1; return 7 }
fun captureText(): String { effects = effects * 10 + 2; return "O" }
fun argument(value: Int): Int { effects = effects * 10 + value; return value }
fun box(): String {
    effects = 0
    val f = make(captureNumber(), captureText())
    if (effects != 12) return "capture effects"
    if (f(argument(3), argument(4)) != "7:O:3:4:9" || effects != 1234) return "first call"
    if (f(argument(5), argument(6)) != "7:O:5:6:9" || effects != 123456) return "second call"
    if (inlineMake(8, "K")(1, 2) != "8:K:1:2:9") return "prepared call"
    if (anonymousMake(10, "A")(2, 3) != "10:A:2:3:9") return "anonymous call"
    return "OK"
}
"#;

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

#[test]
fn official_raw_and_prepared_closures_keep_lexical_frames_and_once_only_operands() {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").map(std::path::PathBuf::from) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    // Establish the portable source behavior before executing a Native dependency consumer.
    common::expect_box_ok_with_stdlib(&format!("{LIBRARY}\n{CONSUMER}"), "SerializedClosureOracle");
    let work = common::scratch_dir().expect("allocate serialized closure producer directory");
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
        let symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), name);
        let [function] = symbols.callables.functions() else {
            panic!("one producer declaration")
        };
        let selected = libraries
            .external_callable(function.callable.external_identity.unwrap())
            .unwrap();
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
        assert_frames(raw.ir(), capturer);
        if name == "inlineMake" {
            let mut prepared = IrFile::default();
            krusty::klib_lowering::lower_inline_template(
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
            assert_frames(&prepared, capturer);
        }
    }
    let consumer_text = format!("package fixture\n{CONSUMER}");
    let input = krusty::source::SourceInput::kotlin(&consumer_text).with_file_stem("Consumer");
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &[input],
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let target = krusty::native::NativeTarget::host().expect("host Native target");
    let backend = krusty::native::CraneliftBackend::new(target)
        .with_entry(krusty::native::Entry::Box)
        .verified();
    let artifacts = krusty::compiler::emit_analyzed(
        analysis,
        &["Consumer".to_owned()],
        &backend,
        "consumer",
        &mut diags,
    );
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diag| diag.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    let objects = artifacts
        .into_iter()
        .filter_map(|(name, bytes)| name.ends_with(".o").then_some(bytes))
        .collect::<Vec<_>>();
    assert!(!objects.is_empty());
    let image = krusty::native::link_program(
        &objects.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        target,
    )
    .unwrap();
    let output =
        common::native_backend::run_native_image(&image, "SerializedClosureConsumer", &[], &[])
            .unwrap_or_else(|failure| panic!("Native consumer: {}", failure.as_failure()));
    assert!(
        output.status.success(),
        "Native consumer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}OK", krusty::backend::BOX_RESULT_FRAME)
    );
    assert_eq!(output.stderr, Vec::<u8>::new());
    std::fs::remove_dir_all(work).unwrap();
}
