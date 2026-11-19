//! PLATFORM-SPECIFIC: the Native consumer imports an official KLIB. The producer builds anonymous
//! objects whose members read properties they inherit from a class or declare themselves, so the
//! KLIB names those getters by file-local signatures. The consumer calls each producer function.
//! The portable program is `tests/box_regressions/inheritedFileLocalGetters.kt`.

use crate::common;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("box_regressions/inheritedFileLocalGetters.kt");

const CONSUMER: &str = "\
fun box(): String {
    if (readInherited() != \"r/null/3/7\") return \"inherited\"
    if (readThroughIntermediate() != \"r/3/7/21\") return \"through an intermediate class\"
    if (readDeclared() != 5) return \"declared\"
    if (readDeclaredAndInherited() != \"33\") return \"declared and inherited\"
    if (readOverridden() != \"21\") return \"overridden\"
    return \"OK\"
}
";

#[test]
fn local_classes_read_the_getters_they_declare_and_inherit() {
    let Some(_) = std::env::var_os("KRUSTY_KOTLIN_NATIVE") else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    let consumer = format!("package fixture\n{CONSUMER}");
    common::expect_box_ok_with_stdlib(LIBRARY, "InheritedFileLocalGettersOracle");
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .map(std::path::PathBuf::from)
        .expect("the caller requires an official Native distribution");
    let work = common::scratch_dir().expect("allocate anonymous-object producer directory");
    let source = work.join("InheritedGetters.kt");
    // The shared program's `box` is the single-module oracle. The producer library exports
    // the readers; this consumer supplies the entry.
    let producer = LIBRARY
        .split_once("\nfun box():")
        .expect("the shared program declares box")
        .0;
    std::fs::write(&source, producer).unwrap();
    let library = work.join("getters");
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
    let output =
        common::native_backend::run_native_image(&image, "InheritedFileLocalGetters", &[], &[])
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
