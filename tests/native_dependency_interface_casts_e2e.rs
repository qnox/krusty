//! A real producer KLIB has interface headers but no Native descriptor definitions in the
//! consumer's emission session. Run the entire shared program with that boundary, not a reduced
//! source implementation or a handwritten descriptor for its private implementation.

use super::common;
use krusty::diag::{DiagSink, Severity};
use krusty::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use krusty::source::SourceInput;

const PROGRAM: &str =
    include_str!("box_regressions/multimodule/selectedDependencyInterfaceCasts.kt");

#[test]
fn selected_klib_interfaces_authenticate_erased_receivers_across_files() {
    let Some(target) = krusty_native::NativeTarget::host() else {
        return;
    };
    if !krusty_native::can_link(target) {
        return;
    }
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE") else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    let root = std::path::PathBuf::from(root);
    let (producer, consumer) = PROGRAM
        .split_once("// MODULE: main(lib)")
        .expect("the whole program has its library boundary");
    let files = consumer
        .split("// FILE: ")
        .skip(1)
        .map(|file| {
            let (name, source) = file.split_once('\n').expect("named consumer file");
            (name.trim_end_matches(".kt"), source)
        })
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    // The whole shared graph is also in the portable inventory. This oracle explicitly compiles
    // the producer as a separate reference JVM library, preserving the library visibility edge.
    let reference_library =
        common::kotlinc_library(producer).expect("reference toolchain required");
    let reference_sources = files
        .iter()
        .map(|(stem, source)| (format!("{stem}.kt"), *source))
        .collect::<Vec<_>>();
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &reference_sources
                .iter()
                .map(|(name, source)| (name.as_str(), *source))
                .collect::<Vec<_>>(),
            "interfacecasts.MainKt",
            &[reference_library],
        ),
        "OK"
    );
    let work = common::scratch_dir().expect("interface-cast producer directory");
    let source = work.join("Interfaces.kt");
    std::fs::write(&source, producer).unwrap();
    let library = work.join("interfaces");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(source)
        .output()
        .expect("SelectedDependencyInterfaceCasts: run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "SelectedDependencyInterfaceCasts: Native KLIB producer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (libraries, imports) = krusty_native::NativeKlibInputs::open(
        &[
            KlibLibraryInput::dependency(library),
            KlibLibraryInput::platform_stdlib(
                root.join("klib/common/stdlib"),
                KlibStdlibSemantics::Native,
            ),
        ],
        target,
    )
    .expect("read real Native producer and stdlib KLIBs")
    .into_parts();
    let inputs = files
        .iter()
        .map(|(stem, source)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = files
        .iter()
        .map(|(stem, _)| stem.to_string())
        .collect::<Vec<_>>();
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries),
        ),
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let backend = krusty_native::CraneliftBackend::new(target)
        .with_entry(krusty_native::Entry::Box)
        .verified();
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, "consumer", &mut diags);
    assert_eq!(
        diags
            .diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    let objects = artifacts
        .iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, bytes)| bytes.as_slice())
        .collect::<Vec<_>>();
    assert!(!objects.is_empty());
    let image = krusty_native::link_program_with_libraries(&objects, &imports, target).unwrap();
    let output = common::native_backend::run_native_image(
        &image,
        "SelectedDependencyInterfaceCasts",
        &[],
        &[],
    )
    .unwrap_or_else(|failure| panic!("Native consumer: {}", failure.as_failure()));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        format!("{}OK", krusty::backend::BOX_RESULT_FRAME).as_bytes()
    );
    assert_eq!(output.stderr, Vec::<u8>::new());
    std::fs::remove_dir_all(work).unwrap();
}
