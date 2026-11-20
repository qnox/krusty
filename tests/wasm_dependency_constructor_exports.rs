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
    assert_eq!(
        modules[1].files.len(),
        2,
        "the later checked source must reuse the allocation export"
    );
    run_against_official_producer(source, "DependencyConstructorAllocation");
}

/// A public final class's members are lowered from the official producer's bodies: the consumer
/// constructs it, calls its function and reads and writes its properties.
#[test]
fn wasm_official_dependency_class_members_lower_from_their_bodies() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyClassMembers.kt"),
        "DependencyClassMembers",
    );
}

#[test]
fn wasm_official_dependency_nested_classes_lower_from_their_bodies() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyNestedClasses.kt"),
        "DependencyNestedClasses",
    );
}

/// A final class's secondary constructors delegate to its primary one: each construction lowers
/// from the official producer's bodies, the delegated initialization first.
#[test]
fn wasm_official_dependency_secondary_constructors_delegate_to_the_primary() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencySecondaryConstructors.kt"),
        "DependencySecondaryConstructors",
    );
}

/// Secondary constructors that reach the primary one through another secondary constructor lower
/// from the official producer's bodies, each running its own body after the one it delegates to.
#[test]
fn wasm_official_dependency_secondary_constructors_delegate_through_each_other() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyChainedSecondaryConstructors.kt"),
        "DependencyChainedSecondaryConstructors",
    );
}

/// Generic classes, a generic superclass and a secondary constructor of a generic class lower
/// from the official producer's bodies.
#[test]
fn wasm_official_dependency_generic_classes_lower_from_their_bodies() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyGenericClasses.kt"),
        "DependencyGenericClasses",
    );
}

/// Library bodies with a generic private function call, a `super` call, a nested class reading
/// its outer class's private property, a generic private member and reordered named arguments
/// lower from the official producer's bodies.
#[test]
fn wasm_official_dependency_body_member_forms_lower_from_their_bodies() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyBodyMemberForms.kt"),
        "DependencyBodyMemberForms",
    );
}

/// A library body calls generic members of a generic class and of a class private to its
/// library, binding the member's own type parameters and its class's.
#[test]
fn wasm_official_dependency_generic_members_of_library_classes_bind_every_type_parameter() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyGenericMembersOfLibraryClasses.kt"),
        "DependencyGenericMembersOfLibraryClasses",
    );
}

/// A library interface's member-extension default called through the interface: the product
/// producer exports an entry dispatching on the receiver, so the shared program passes; the
/// official producer exports none, so the consumer refuses the call before it could run the
/// published default in place of an override.
#[test]
fn wasm_official_dependency_interface_member_extension_without_dispatch_entry_is_refused() {
    let source = include_str!("box_regressions/libraryInterfaceMemberExtensionDefaults.kt");
    for target in WasmTarget::ALL {
        let consumer =
            compile_against_official_producer(source, "InterfaceMemberExtensionDefaults", target);
        let diagnostics = consumer
            .diags
            .diags
            .iter()
            .map(|diag| {
                (
                    diag.severity,
                    format!("{:?}", diag.kind),
                    (diag.file, diag.span.lo, diag.span.hi),
                    diag.msg.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            diagnostics,
            [(
                krusty::diag::Severity::Error,
                "Compiler".to_owned(),
                (0, 0, 0),
                "krusty: the wasm backend does not support a call through the dependency \
                 interface member extension `word` without an entry dispatching on its receiver yet",
            )],
            "{target:?}"
        );
        std::fs::remove_dir_all(consumer.work).unwrap();
    }
}

/// Run the shared two-module program through the product producer, then compile its consumer
/// against the library the exact official Wasm compiler produces and run it, on every target.
fn run_against_official_producer(source: &str, name: &str) {
    for target in WasmTarget::ALL {
        let consumer = compile_against_official_producer(source, name, target);
        assert!(
            consumer.diags.diags.is_empty(),
            "{:#?}",
            consumer.diags.diags
        );
        let outcome = consumer.lane.run_artifacts(
            &consumer.work,
            &format!("official{name}"),
            "import",
            source,
            box_lane::CompiledProgram {
                artifacts: consumer.artifacts,
                native_imports: Vec::new(),
            },
        );
        assert_eq!(outcome, Outcome::Pass);
        std::fs::remove_dir_all(consumer.work).unwrap();
    }
}

/// The consumer of the shared two-module program, compiled against the official producer's
/// library after the product producer's whole program passed on the same target.
struct OfficialConsumer {
    lane: WasmLane,
    work: std::path::PathBuf,
    diags: DiagSink,
    artifacts: Vec<krusty::compiler::Artifact>,
}

fn compile_against_official_producer(
    source: &str,
    name: &str,
    target: WasmTarget,
) -> OfficialConsumer {
    let options = krusty::conformance::WasmCompilerOptions::from_source(source)
        .expect("the original module graph's compilation options");
    let modules =
        krusty::conformance::split_modules(source).expect("the entire shared two-module program");
    let units = krusty::conformance::module_units(&modules);
    let [producer, consumer] = modules.as_slice() else {
        panic!("producer and consumer modules")
    };
    assert_eq!(consumer.deps, std::slice::from_ref(&producer.name));
    let [(producer_stem, producer_text)] = producer.files.as_slice() else {
        panic!("one complete producer")
    };
    let lane = WasmLane { target };
    assert!(focused_wasm_lane_available(
        &lane,
        &format!("official{name}")
    ));
    let backend = lane
        .backend(source)
        .expect("the module graph's Wasm configuration");
    let compilation_target = backend.compilation_target();
    let work = crate::common::scratch_dir().expect("official dependency producer directory");
    assert_eq!(
        lane.compile_modules_and_run(&work, &format!("product{name}"), &units, source,),
        Outcome::Pass,
        "the product producer's program passes"
    );
    let producer_path = work.join(format!("{producer_stem}.kt"));
    std::fs::write(&producer_path, producer_text).unwrap();
    let reference_arguments = options
        .reference_arguments(compilation_target)
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect::<Vec<_>>();
    let (produced, library) = crate::reference_invocation_support::run_reference(
        compilation_target,
        &work,
        &[producer_path],
        &reference_arguments,
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
        .map(|(stem, _)| stem.clone())
        .collect::<Vec<_>>();
    let inputs = consumer
        .files
        .iter()
        .zip(&stems)
        .map(|((_, source), stem)| krusty::source::SourceInput::kotlin(source).with_file_stem(stem))
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
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "box", &mut diags);
    OfficialConsumer {
        lane,
        work,
        diags,
        artifacts,
    }
}
