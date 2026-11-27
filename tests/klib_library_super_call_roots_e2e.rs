//! PLATFORM-SPECIFIC: the Native consumer imports an official KLIB. The producer calls interface
//! defaults only through `super` calls that sit in an anonymous class and in a top-level property
//! initializer, each of which overrides the default; the consumer calls the producer's functions.
//! The portable program is `tests/box_regressions/librarySuperCallRoots.kt`.

use crate::common;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const PROGRAM: &str = include_str!("box_regressions/librarySuperCallRoots.kt");

#[test]
fn a_super_call_in_a_library_anonymous_class_or_initializer_runs_the_interface_default() {
    let Some(_) = std::env::var_os("KRUSTY_KOTLIN_NATIVE") else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    // The shared program is two modules: the producer library and the consumer that supplies
    // `box`. Its single-module form is the JVM oracle.
    let (library_module, consumer_module) = PROGRAM
        .split_once("// MODULE: main(lib)\n")
        .expect("the shared program declares a consumer module");
    let producer = library_module.trim_start_matches("// MODULE: lib\n");
    let consumer = format!("package fixture\n{consumer_module}");
    common::expect_box_ok_with_stdlib(
        &format!("{producer}\n{consumer_module}"),
        "LibrarySuperCallRootsOracle",
    );
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .map(std::path::PathBuf::from)
        .expect("the caller requires an official Native distribution");
    let work = common::scratch_dir().expect("allocate super-call producer directory");
    let source = work.join("SuperCallRoots.kt");
    std::fs::write(&source, format!("package fixture\n{producer}")).unwrap();
    let library = work.join("superCalls");
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
    let inputs = [krusty::source::SourceInput::kotlin(&consumer).with_file_stem("Consumer")];
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
    let output =
        common::native_backend::run_native_image(&image, "LibrarySuperCallRoots", &[], &[])
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
