//! PLATFORM-SPECIFIC: official Wasm KLIB production and the Wasm artifact runner exercise imports.
//! The complete portable Kotlin input remains in the shared multi-module corpus.

use super::*;
use krusty::backend::Backend;
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

#[test]
fn wasm_official_dependency_constructor_roots_allocate_and_rebind() {
    let source =
        include_str!("box_regressions/multimodule/dependencyConstructorAllocationExports.kt");
    let modules =
        krusty::conformance::split_modules(source).expect("the entire shared two-module program");
    let units = krusty::conformance::module_units(&modules);
    let [producer, consumer] = modules.as_slice() else {
        panic!("producer and consumer modules")
    };
    assert_eq!(consumer.deps, std::slice::from_ref(&producer.name));
    let [(producer_name, producer_text)] = producer.files.as_slice() else {
        panic!("one complete producer")
    };
    assert_eq!(
        consumer.files.len(),
        2,
        "the later checked source must reuse the allocation export"
    );
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(focused_wasm_lane_available(
            &lane,
            "officialDependencyConstructorAllocation"
        ));
        let backend = WasmBackend::new(target).with_entry(Entry::Box);
        let compilation_target = backend.compilation_target();
        let work = crate::common::scratch_dir().expect("official dependency producer directory");
        assert_eq!(
            lane.compile_modules_and_run(
                &work,
                "productDependencyConstructorAllocation",
                &units,
                source,
            ),
            Outcome::Pass,
            "the product producer's existing constructor export remains callable"
        );
        let producer_path = work.join(producer_name);
        std::fs::write(&producer_path, producer_text).unwrap();
        let (produced, library) = crate::reference_invocation_support::run_reference(
            compilation_target,
            &work,
            &[producer_path],
            &[],
            false,
        )
        .expect("the exact Wasm reference compiler is provisioned");
        assert_eq!(produced.status, 0, "official producer: {}", produced.stderr);
        let libraries = KlibLibraries::open(&[
            KlibLibraryInput::platform_stdlib(
                krusty::toolchain::kotlin_stdlib_klib(compilation_target)
                    .expect("the exact target stdlib"),
                KlibStdlibSemantics::Wasm,
            ),
            KlibLibraryInput::dependency(library),
        ])
        .expect("the actual official producer artifact and target stdlib");
        let stems = consumer
            .files
            .iter()
            .map(|(name, _)| name.strip_suffix(".kt").expect("Kotlin source").to_owned())
            .collect::<Vec<_>>();
        let inputs = consumer
            .files
            .iter()
            .zip(&stems)
            .map(|((_, source), stem)| {
                krusty::source::SourceInput::kotlin(source).with_file_stem(stem)
            })
            .collect::<Vec<_>>();
        let mut diags = DiagSink::new();
        let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
            &inputs,
            krusty::frontend::PlatformProvider::new(
                compilation_target,
                Box::new(libraries.for_compilation()),
            ),
            &krusty::features::LangFeatures::new(),
            &mut diags,
        );
        let artifacts =
            krusty::compiler::emit_analyzed(analysis, &stems, &backend, "box", &mut diags);
        assert!(diags.diags.is_empty(), "{:#?}", diags.diags);
        let outcome = lane.run_artifacts(
            &work,
            "officialDependencyConstructorAllocation",
            "import",
            box_lane::CompiledProgram {
                artifacts,
                native_imports: Vec::new(),
            },
        );
        assert_eq!(outcome, Outcome::Pass);
        std::fs::remove_dir_all(work).unwrap();
    }
}
