//! PLATFORM-SPECIFIC: the Native consumer imports an official KLIB. The producer's anonymous
//! object inherits an interface default that no body calls, and the producer calls the member
//! through the interface the default implements, so only dispatch reaches the default. The
//! portable program is `tests/box_regressions/libraryAnonymousObjectInheritedDefault.kt`, whose
//! Wasm counterpart runs in `tests/wasm_dependency_constructor_exports.rs`.

use crate::common;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const PROGRAM: &str = include_str!("box_regressions/libraryAnonymousObjectInheritedDefault.kt");

#[test]
fn an_anonymous_object_carries_the_interface_default_only_dispatch_reaches() {
    let Some(_) = std::env::var_os("KRUSTY_KOTLIN_NATIVE") else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    let modules = krusty::conformance::split_modules(PROGRAM).expect("the shared two modules");
    let [library_module, consumer_module] = modules.as_slice() else {
        panic!("the library and its consumer")
    };
    let [(_, producer)] = library_module.files.as_slice() else {
        panic!("one library file")
    };
    let [(_, consumer)] = consumer_module.files.as_slice() else {
        panic!("one consumer file")
    };
    common::expect_box_ok_against_ref("LibraryAnonymousObjectInheritedDefault", producer, consumer);
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .map(std::path::PathBuf::from)
        .expect("the caller requires an official Native distribution");
    let work = common::scratch_dir().expect("allocate inherited-default producer directory");
    let source = work.join("AnonymousObject.kt");
    std::fs::write(&source, producer).unwrap();
    let library = work.join("keyed");
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
    .expect("read the serialized library");
    let inputs = [krusty::source::SourceInput::kotlin(consumer).with_file_stem("Consumer")];
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::from_environment(),
        &mut diags,
    );
    let target = krusty_native::NativeTarget::host().expect("host Native target");
    let backend = krusty_native::CraneliftBackend::new(target)
        .with_entry(krusty_native::Entry::Box)
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
    let image = krusty_native::link_program(
        &objects.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        target,
    )
    .unwrap();
    let output = common::native_backend::run_native_image(
        &image,
        "LibraryAnonymousObjectInheritedDefault",
        &[],
        &[],
    )
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
