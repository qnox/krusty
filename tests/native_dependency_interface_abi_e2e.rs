//! Applied primitive and value-class calls cross a real dependency interface's declared frame.
//! The whole program is one owning Native ABI control, with a separately produced JVM library
//! and a genuine Native KLIB header. Implementations and call sites are separate consumer files,
//! so the caller must import the interface slot instead of selecting a local implementor arm.
//! The same complete program is in the shared portable inventory. This owning artifact test
//! additionally requires a real Native producer archive and import-aware Native linkage.

use super::common;
use krusty::diag::{DiagSink, Severity};
use krusty::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use krusty::source::SourceInput;
use object::{Object, ObjectSection};

const PROGRAM: &str = include_str!("box_regressions/multimodule/dependencyInterfaceAppliedAbi.kt");

// PLATFORM-SPECIFIC: genuine Native KLIB headers and import-aware binary ABI; the whole Kotlin
// behavior program remains in the shared portable inventory.
#[test]
fn a_declared_generic_slot_converts_int_long_and_value_class_arguments_and_results() {
    let Some(target) = krusty_native::NativeTarget::host() else {
        return;
    };
    if !krusty_native::can_link(target) {
        return;
    }
    let Some(root) = crate::reference_recording_support::kotlin_native_root() else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    let native =
        krusty::conformance::prepare_test_source(PROGRAM, krusty::conformance::TestTarget::Native);
    let modules = krusty::conformance::split_modules(&native).expect("whole Native module graph");
    let units = krusty::conformance::module_units(&modules);
    let [producer, consumer] = units.as_slice() else {
        panic!("one producer and one consumer unit");
    };
    assert_eq!(producer.files.len(), 1);
    assert_eq!(consumer.files.len(), 2);
    assert_eq!(
        consumer.deps.as_slice(),
        std::slice::from_ref(&producer.name)
    );
    let files = consumer
        .files
        .iter()
        .map(|(stem, source)| (stem.as_str(), source.as_str()))
        .collect::<Vec<_>>();

    // Use the same canonical target preparation as the portable matrix, including the optional
    // JVM value-class annotation; every producer and consumer file is retained whole.
    let jvm =
        krusty::conformance::prepare_test_source(PROGRAM, krusty::conformance::TestTarget::Jvm);
    let modules = krusty::conformance::split_modules(&jvm).expect("whole JVM module graph");
    let units = krusty::conformance::module_units(&modules);
    let [reference_producer, reference_consumer] = units.as_slice() else {
        panic!("whole reference graph");
    };
    assert_eq!(reference_producer.files.len(), 1);
    assert_eq!(reference_consumer.files.len(), 2);
    let reference_library = common::kotlinc_library(&reference_producer.files[0].1)
        .expect("reference toolchain required");
    let reference_sources = reference_consumer
        .files
        .iter()
        .map(|(stem, source)| (format!("{stem}.kt"), source.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &reference_sources
                .iter()
                .map(|(name, source)| (name.as_str(), *source))
                .collect::<Vec<_>>(),
            "boundary.MainKt",
            &[reference_library],
        ),
        "OK"
    );
    let work = common::scratch_dir().expect("interface-ABI producer directory");
    let source = work.join("Slot.kt");
    std::fs::write(&source, &producer.files[0].1).unwrap();
    let library = work.join("interface_abi");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(source)
        .output()
        .expect("DependencyInterfaceAppliedAbi: run exact Native KLIB producer");
    assert!(
        output.status.success(),
        "DependencyInterfaceAppliedAbi: Native KLIB producer failed: {}",
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
    // The producer's method is abstract: its module has no executable export to seed this
    // publication. Both real consumer implementations must still publish an identity record,
    // with descriptor and callable relocations, before the linker can install their adapters.
    let mut records = 0;
    for bytes in &objects {
        let object = object::File::parse(*bytes).expect("consumer object");
        for section in object.sections().filter(|section| {
            section.name().ok() == Some(".kt_interface_dependency_implementations")
        }) {
            let data = section.data().expect("implementation record bytes");
            let mut at = 0;
            let mut count = 0;
            while at < data.len() {
                let identity = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
                let identity = usize::try_from(identity).unwrap();
                at += 24 + identity.div_ceil(8) * 8;
                count += 1;
            }
            assert_eq!(at, data.len(), "complete implementation records");
            assert_eq!(
                section.relocations().count(),
                count * 2,
                "each implementation names an actual descriptor and function"
            );
            records += count;
        }
    }
    assert_eq!(
        records, 2,
        "IntSlot and ErasedSlot publish the abstract dependency member"
    );
    let image = krusty_native::link_program_with_libraries(&objects, &imports, target).unwrap();
    let output =
        common::native_backend::run_native_image(&image, "DependencyInterfaceAppliedAbi", &[], &[])
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
