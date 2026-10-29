//! PLATFORM: serialized KLIB closure frames, prepared-inline copies and the Native closure ABI.

use crate::common;
use krusty::backend::{Artifact, Backend, CheckedIrFile, CheckedIrUnitKind};
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("fixtures/klib_closure_frames/Library.kt");

const CONSUMER: &str = include_str!("fixtures/klib_closure_frames/Consumer.kt");

const CONSTRUCTOR_CONSUMER: &str = r#"
fun box(): String {
    val f = makeWithConstructor(40)
    return if (f() == 41 && f() == 41) "OK" else "wrong closure result"
}
"#;

#[test]
fn official_dependency_closures_preserve_nested_values_and_once_only_operands() {
    assert_official_dependency_closures(
        LIBRARY,
        CONSUMER,
        "SerializedClosure",
        DependencyConsumer::Native,
    );
}

#[test]
fn dependency_top_level_callable_references_keep_declaration_identity_and_invocation() {
    let library = include_str!("fixtures/klib_closure_frames/TopLevelReferences.kt");
    let consumer = include_str!("fixtures/klib_closure_frames/TopLevelReferenceConsumer.kt");
    let dependency = common::kotlinc_lib_out(&[("Library.kt", library)])
        .expect("reference compiler required for callable-reference oracle");
    let packaged_consumer = format!("package fixture\n{consumer}");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[("Consumer.kt", &packaged_consumer)],
            "fixture.ConsumerKt",
            &[dependency],
        ),
        "OK"
    );
    assert_official_dependency_closures(
        library,
        consumer,
        "SerializedTopLevelReference",
        DependencyConsumer::Native,
    );
}

#[test]
fn a_dependency_constructor_used_only_inside_nested_closures_is_prepared() {
    let library = include_str!("fixtures/klib_closure_frames/DependencyCalls.kt");
    // Imported class storage is an independent Native capability. Observe the actual checked
    // dependency-support IR, with Native's preparation capabilities, before target realization.
    assert_official_dependency_closures(
        library,
        CONSTRUCTOR_CONSUMER,
        "SerializedClosureDependencies",
        DependencyConsumer::ConstructorIdentity,
    );
}

#[test]
fn a_dependency_constructor_used_only_inside_nested_closures_executes() {
    assert_official_dependency_closures(
        include_str!("fixtures/klib_closure_frames/DependencyCalls.kt"),
        CONSTRUCTOR_CONSUMER,
        "ImportedClassConstructor",
        DependencyConsumer::Native,
    );
}

#[test]
fn dependency_class_initializers_run_in_order_for_each_closure_construction() {
    assert_official_dependency_closures(
        include_str!("fixtures/klib_dependency_classes/Initialized.kt"),
        include_str!("fixtures/klib_dependency_classes/InitializedConsumer.kt"),
        "ImportedClassInitialization",
        DependencyConsumer::Native,
    );
}

#[test]
fn dependency_class_storage_is_independent_of_constructor_parameters() {
    assert_official_dependency_closures(
        include_str!("fixtures/klib_dependency_classes/Initialized.kt"),
        include_str!("fixtures/klib_dependency_classes/InitializedConsumer.kt"),
        "ImportedClassLayout",
        DependencyConsumer::InitializationLayout,
    );
}

#[test]
fn dependency_class_cannot_consume_same_name_source_classifier_facts() {
    let consumer = format!(
        "class BodyOnly(seed: Int) {{ val replacement: Long = seed.toLong() }}\n{CONSTRUCTOR_CONSUMER}"
    );
    assert_official_dependency_closures(
        include_str!("fixtures/klib_closure_frames/DependencyCalls.kt"),
        &consumer,
        "ImportedClassSourceCollision",
        DependencyConsumer::SourceClassifierCollision,
    );
}

#[test]
fn a_runtime_constructor_used_only_inside_nested_dependency_closures_executes() {
    let library = include_str!("fixtures/klib_closure_frames/RuntimeConstructor.kt");
    let consumer = r#"
fun box(): String {
    val f = makeWithRuntimeConstructor(40)
    return if (f() == 41 && f() == 41) "OK" else "wrong closure result"
}
"#;
    assert_official_dependency_closures(
        library,
        consumer,
        "SerializedClosureRuntimeConstructor",
        DependencyConsumer::Native,
    );
}

#[test]
fn serialized_runtime_accessor_leaves_preserve_pair_members() {
    let library = r#"package fixture
fun pickPair(pair: Pair<String, String>): String = pair.first + pair.second
"#;
    let consumer = r#"
fun box(): String = pickPair(Pair("O", "K"))
"#;
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[
                ("Library.kt", library),
                ("Consumer.kt", &format!("package fixture\n{consumer}")),
            ],
            "fixture.ConsumerKt",
            &[],
        ),
        "OK",
    );
    assert_official_dependency_closures(
        library,
        consumer,
        "SerializedPairAccessor",
        DependencyConsumer::Native,
    );
}

#[test]
fn serialized_runtime_accessor_leaves_preserve_applied_scalar_and_nullable_results() {
    let library = r#"package fixture
fun pickPairIndex(pair: Pair<Int, String?>): Int = pair.first
fun pickPairValue(pair: Pair<Int, String?>): String? = pair.second
"#;
    let consumer = r#"
fun box(): String {
    if (pickPairIndex(Pair(7, null)) != 7) return "scalar first"
    if (pickPairValue(Pair(7, null)) != null) return "nullable second"
    return pickPairValue(Pair(7, "OK")) ?: "missing second"
}
"#;
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[
                ("Library.kt", library),
                ("Consumer.kt", &format!("package fixture\n{consumer}")),
            ],
            "fixture.ConsumerKt",
            &[],
        ),
        "OK",
    );
    assert_official_dependency_closures(
        library,
        consumer,
        "SerializedAppliedPairAccessor",
        DependencyConsumer::Native,
    );
}

enum DependencyConsumer {
    Native,
    ConstructorIdentity,
    InitializationLayout,
    SourceClassifierCollision,
}

/// Observe the same common preparation Native requests without claiming its imported-class
/// storage implementation can execute the construction. Every capability is delegated to Native.
struct ConstructorIdentityProbe {
    native: krusty::native::CraneliftBackend,
    producer_module: String,
    initialization_layout: bool,
}

impl Backend for ConstructorIdentityProbe {
    type State = usize;

    fn compilation_target(&self) -> krusty::compilation_target::CompilationTarget {
        self.native.compilation_target()
    }

    fn emits_dependency_support(&self) -> bool {
        self.native.emits_dependency_support()
    }

    fn numbers_slots(&self) -> bool {
        self.native.numbers_slots()
    }

    fn realizes_bodyless_dependency_support(&self) -> bool {
        self.native.realizes_bodyless_dependency_support()
    }

    fn realizes_dependency_intrinsic(
        &self,
        operation: krusty::backend::BackendCompilerIntrinsic,
    ) -> bool {
        self.native.realizes_dependency_intrinsic(operation)
    }

    fn realizes_dependency_getter(
        &self,
        signature: krusty::backend::DependencyGetterSignature<'_>,
    ) -> bool {
        self.native.realizes_dependency_getter(signature)
    }

    fn same_module_inlining(&self) -> krusty::ir::SameModuleInlining {
        self.native.same_module_inlining()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        count: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        use krusty::types::Ty;
        let expected = krusty::types::type_name(if self.initialization_layout {
            "fixture/Initialized"
        } else {
            "fixture/BodyOnly"
        });
        for expression in &file.ir.exprs {
            let krusty::ir::IrExpr::New {
                internal,
                external_target,
                ..
            } = expression
            else {
                continue;
            };
            if *internal != expected {
                continue;
            }
            assert_eq!(file.kind, CheckedIrUnitKind::DependencySupport);
            let fact = file
                .callables
                .callable(external_target.expect("prepared constructor handle"))
                .expect("authenticated constructor facts");
            assert_eq!(
                fact.kind,
                krusty::libraries::ExternalCallableKind::Constructor
            );
            assert_eq!(
                fact.declaration_owner,
                Some(krusty::types::SemanticCallableOwner::Classifier(expected))
            );
            let params = if self.initialization_layout {
                vec![Ty::Int, Ty::fun(vec![Ty::Int], Ty::Int)]
            } else {
                vec![Ty::Int]
            };
            assert_eq!(fact.params, params);
            let identity = fact
                .dependency_callable
                .as_ref()
                .expect("producer-owned constructor identity");
            assert_eq!(identity.module, self.producer_module);
            assert_eq!(
                fact.declaration_signature.as_ref(),
                Some(&identity.declaration)
            );
            if self.initialization_layout {
                let class = &file.ir.classes[file.ir.class_id_by_name(expected).unwrap() as usize];
                assert_eq!(
                    class
                        .fields
                        .iter()
                        .map(|field| (field.name.as_str(), field.ty))
                        .collect::<Vec<_>>(),
                    vec![("first", Ty::Int), ("tag", Ty::String), ("second", Ty::Int)]
                );
                assert_eq!(
                    class
                        .ctor_args
                        .iter()
                        .map(|arg| (arg.ty, arg.is_field, arg.field_index))
                        .collect::<Vec<_>>(),
                    params
                        .iter()
                        .map(|ty| (*ty, false, None))
                        .collect::<Vec<_>>()
                );
                assert!(class.explicit_param_stores);
                assert_eq!(class.ctor_param_count, 0);
            }
            *count += 1;
        }
        Vec::new()
    }

    fn finalize(&self, count: Self::State, _module_name: &str) -> Vec<Artifact> {
        assert_eq!(count, 1, "one reached nested constructor implementation");
        Vec::new()
    }
}

fn assert_official_dependency_closures(
    library_source: &str,
    consumer: &str,
    fixture_name: &str,
    purpose: DependencyConsumer,
) {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").map(std::path::PathBuf::from) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    };
    // Establish the portable source behavior before executing a Native dependency consumer.
    let consumer_text = format!("package fixture\n{consumer}");
    if matches!(purpose, DependencyConsumer::SourceClassifierCollision) {
        // These are different modules with equal classifier names, not a source redeclaration.
        let dependency = common::kotlinc_lib_out(&[("Library.kt", library_source)])
            .expect("reference compiler required for dependency identity oracle");
        assert_eq!(
            common::kotlinc_box_files_result_with_classpath(
                &[("Consumer.kt", &consumer_text)],
                "fixture.ConsumerKt",
                &[dependency],
            ),
            "OK"
        );
    } else {
        common::expect_box_ok_with_stdlib(
            &format!("{library_source}\n{consumer}"),
            &format!("{fixture_name}Oracle"),
        );
    }
    let work = common::scratch_dir().expect("allocate serialized closure producer directory");
    let source = work.join("Frames.kt");
    std::fs::write(&source, library_source).unwrap();
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
    let manifest = krusty::klib::KlibArchive::open(&library)
        .unwrap()
        .manifest()
        .unwrap();
    let libraries = KlibLibraries::open(&[
        KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            KlibStdlibSemantics::Native,
        ),
        KlibLibraryInput::dependency(library),
    ])
    .expect("read official serialized closure declarations");
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
    let stems = ["Consumer".to_owned()];
    let artifacts = match purpose {
        DependencyConsumer::Native | DependencyConsumer::SourceClassifierCollision => {
            krusty::compiler::emit_analyzed(analysis, &stems, &backend, "consumer", &mut diags)
        }
        DependencyConsumer::ConstructorIdentity | DependencyConsumer::InitializationLayout => {
            krusty::compiler::emit_analyzed(
                analysis,
                &stems,
                &ConstructorIdentityProbe {
                    native: backend,
                    producer_module: manifest.unique_name().unwrap().to_owned(),
                    initialization_layout: matches!(
                        purpose,
                        DependencyConsumer::InitializationLayout
                    ),
                },
                "consumer",
                &mut diags,
            )
        }
    };
    if matches!(purpose, DependencyConsumer::SourceClassifierCollision) {
        assert_eq!(
            diags.diags.iter().map(|diag| (
                diag.file, diag.span, diag.editor_span, diag.severity, diag.warning_name,
                diag.kind, diag.identity, diag.msg.as_str(),
            )).collect::<Vec<_>>(),
            vec![(
                0, krusty::diag::Span::new(0, 0), None, krusty::diag::Severity::Error, None,
                krusty::diag::DiagnosticKind::Compiler, None,
                "cannot materialize a dependency class that collides with a current-module classifier",
            )]
        );
        assert!(artifacts.is_empty());
        std::fs::remove_dir_all(work).unwrap();
        return;
    }
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diag| diag.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    if matches!(
        purpose,
        DependencyConsumer::ConstructorIdentity | DependencyConsumer::InitializationLayout
    ) {
        assert!(artifacts.is_empty());
        std::fs::remove_dir_all(work).unwrap();
        return;
    }
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
    let output = common::native_backend::run_native_image(
        &image,
        &format!("{fixture_name}Consumer"),
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
