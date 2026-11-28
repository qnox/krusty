//! PLATFORM-SPECIFIC: the KLIB forms kotlinc-native serializes for an atomicfu library.
//!
//! The atomicfu compiler plugin rewrites an inline extension of an atomic into a helper that takes
//! a lambda returning a reference to the atomic's field, and every operation on the atomic in its
//! body into the Native field intrinsic on that lambda's result. A producer that inlines before it
//! serializes leaves each caller holding the helper's inlined body, in which the lambda's
//! invocation is a returnable block returning the property reference. One that does not (any
//! producer before Kotlin 2.4.20, or `-Xklib-ir-inliner=disabled`) leaves each caller calling the
//! helper, which its metadata does not publish, with the lambda; the intrinsic's receiver is the
//! lambda's invocation until the call is expanded. A reference atomic is an `Any?` field whose
//! reads reach the atomic's declared type without a cast, and a producer without rich callable
//! references in KLIBs references each field by a plain property reference. The program itself
//! is portable; the JVM oracle runs it against the atomicfu runtime before the Native consumer
//! runs the dependency's serialized bodies.

use crate::common;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("fixtures/klib_atomicfu/Library.kt");
const CONSUMER: &str = include_str!("fixtures/klib_atomicfu/Consumer.kt");

#[test]
fn dependency_atomicfu_helpers_update_the_field_their_inlined_lambda_references() {
    atomicfu_consumer_runs(&[]);
}

#[test]
fn dependency_atomicfu_helper_calls_update_the_field_their_lambda_references() {
    atomicfu_consumer_runs(&["-Xklib-ir-inliner=disabled"]);
}

/// A producer that neither inlines before it serializes nor writes rich callable references, as
/// a Kotlin 2.2 producer made kotlinx-coroutines' published KLIBs, serializes each field
/// reference as a plain property reference.
#[test]
fn dependency_atomicfu_plain_property_references_update_their_field() {
    atomicfu_consumer_runs(&[
        "-Xklib-ir-inliner=disabled",
        "-XXLanguage:-IrRichCallableReferencesInKlibs",
    ]);
}

/// Produce the atomicfu library with `producer_arguments`, then compile and run the consumer
/// against its serialized bodies on Native.
fn atomicfu_consumer_runs(producer_arguments: &[&str]) {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").map(std::path::PathBuf::from) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    let runtime =
        krusty::toolchain::atomicfu_runtime_jar().expect("the atomicfu runtime is provisioned");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[("Library.kt", LIBRARY), ("Consumer.kt", CONSUMER)],
            "fixture.ConsumerKt",
            &[runtime],
        ),
        "OK",
        "unexpected kotlinc result"
    );

    let plugin = krusty::toolchain::atomicfu_compiler_plugin_jar()
        .expect("the atomicfu compiler plugin is provisioned");
    let atomicfu =
        krusty::toolchain::atomicfu_klib(krusty::compilation_target::CompilationTarget::Native)
            .unwrap_or_else(|reason| panic!("{reason}"))
            .expect("Native has an atomicfu runtime KLIB");
    let work = common::scratch_dir().expect("allocate atomicfu producer directory");
    let source = work.join("Library.kt");
    std::fs::write(&source, LIBRARY).unwrap();
    let library = work.join("counter");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .arg(format!("-Xplugin={}", plugin.display()))
        .arg("-l")
        .arg(&atomicfu)
        .args(producer_arguments)
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
        KlibLibraryInput::dependency(atomicfu),
        KlibLibraryInput::dependency(library),
    ])
    .expect("read the atomicfu dependency declarations");
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &[krusty::source::SourceInput::kotlin(CONSUMER).with_file_stem("Consumer")],
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::new(),
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
    let output = common::native_backend::run_native_image(&image, "AtomicfuConsumer", &[], &[])
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
