//! PLATFORM: serialized KLIB closure frames, prepared-inline copies and the Native closure ABI.

use crate::common;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("fixtures/klib_closure_frames/Library.kt");

const CONSUMER: &str = include_str!("fixtures/klib_closure_frames/Consumer.kt");

#[test]
fn official_dependency_closures_preserve_nested_values_and_once_only_operands() {
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
