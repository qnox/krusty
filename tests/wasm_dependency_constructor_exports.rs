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

/// A library function that tests a value against a file-private interface of its library and calls
/// that interface's member lowers from the official producer's body through the interface's
/// installed class.
#[test]
fn wasm_official_dependency_file_private_interface_dispatches_through_its_installed_class() {
    run_against_official_producer(
        include_str!(
            "box_regressions/multimodule/dependencyInstalledInterfaceTypeTestAndMemberCall.kt"
        ),
        "DependencyInstalledInterfaceTypeTestAndMemberCall",
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

/// A library body calls a member an abstract class inherits from several interfaces without a
/// body; the most specific inherited declaration, by its typed result, is the one it calls.
#[test]
fn wasm_official_dependency_inherited_abstract_members_select_the_most_specific() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyInheritedAbstractMembers.kt"),
        "DependencyInheritedAbstractMembers",
    );
}

/// The most specific inherited declaration is selected by subtyping its result: a covariant type
/// argument of an `out` classifier, and an invariant argument against a use-site projection.
#[test]
fn wasm_official_dependency_inherited_covariant_results_select_by_variance() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyInheritedCovariantResults.kt"),
        "DependencyInheritedCovariantResults",
    );
}

/// A library body reads a property of a generic class of its library at a concrete type argument,
/// and the read's value has that type.
#[test]
fn wasm_official_dependency_generic_property_reads_have_the_applied_type() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyGenericPropertyReads.kt"),
        "DependencyGenericPropertyReads",
    );
}

/// A library body reads a generic extension property at the receiver's type argument.
#[test]
fn wasm_official_dependency_generic_extension_properties_bind_their_type_parameters() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyGenericExtensionProperties.kt"),
        "DependencyGenericExtensionProperties",
    );
}

/// A library body appends an `Any?` to a `StringBuilder` by its `toString()`.
#[test]
fn wasm_official_dependency_string_builder_appends_any_value() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyStringBuilderAppendAny.kt"),
        "DependencyStringBuilderAppendAny",
    );
}

/// A library body constructs a generic class through the secondary constructor it selected,
/// although the call's instantiated parameter types differ from the declared ones.
#[test]
fn wasm_official_dependency_generic_secondary_construction_runs_the_selected_constructor() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyGenericSecondaryConstruction.kt"),
        "DependencyGenericSecondaryConstruction",
    );
}

/// An interface member a class reaches only through a supertype is implemented by the member
/// its superclass inherits, for public classes and for classes private to the library.
#[test]
fn wasm_official_dependency_inherited_members_implement_indirect_interfaces() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyInheritedInterfaceImplementations.kt"),
        "DependencyInheritedInterfaceImplementations",
    );
}

/// A later checked source realizes a subclass of a library class an earlier one already realized
/// and exported: the subclass inherits the superclass's members from the earlier realization, as
/// its own interface implementations, instead of realizing the superclass again.
#[test]
fn wasm_official_dependency_subclasses_inherit_members_realized_by_an_earlier_source() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyCrossUnitInheritedMembers.kt"),
        "DependencyCrossUnitInheritedMembers",
    );
}

/// A later source calls a final member of a class an earlier source realized: the class keeps its
/// one owner and the member is emitted for the later source as a function of the receiver.
#[test]
fn wasm_official_dependency_calls_final_members_of_a_class_an_earlier_source_realized() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyCrossUnitFinalMembers.kt"),
        "DependencyCrossUnitFinalMembers",
    );
}

/// A library body reads an abstract accessor of a class an earlier source realized: the read
/// dispatches through that class's frozen declaration rather than declaring the accessor again.
#[test]
fn wasm_official_dependency_reads_abstract_accessors_of_a_class_an_earlier_source_realized() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyCrossUnitAbstractAccessor.kt"),
        "DependencyCrossUnitAbstractAccessor",
    );
}

/// A subclass implements a library interface member with the body it inherits from its
/// superclass, which does not declare the interface and which no body calls.
#[test]
fn wasm_official_dependency_subclasses_implement_interfaces_with_inherited_bodies() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyInheritedSuperclassImplementation.kt"),
        "DependencyInheritedSuperclassImplementation",
    );
}

/// A public constructor that delegates to a library-private primary constructor lowers from the
/// official producer's bodies, though no provider publishes the private constructor to source.
#[test]
fn wasm_official_dependency_public_constructors_delegate_to_a_private_primary() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyPrivatePrimaryConstructor.kt"),
        "DependencyPrivatePrimaryConstructor",
    );
}

/// A library body that asserts a value of a nullable-bounded type parameter non-null (`item!!`)
/// types the assertion `T & Any`, and lowers from the official producer's bodies.
#[test]
fn wasm_official_dependency_definitely_non_null_assertions_lower_from_their_bodies() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyDefinitelyNonNullAssertion.kt"),
        "DependencyDefinitelyNonNullAssertion",
    );
}

/// Two files of the consumer each install a different file-private interface of the library:
/// their members take distinct module-wide numbers, so each call reaches its own member.
#[test]
fn wasm_official_dependency_interfaces_installed_by_two_files_keep_distinct_numbers() {
    run_against_official_producer(
        include_str!("box_regressions/multimodule/dependencyInstalledInterfacesInTwoFiles.kt"),
        "DependencyInstalledInterfacesInTwoFiles",
    );
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
