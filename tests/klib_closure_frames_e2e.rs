//! PLATFORM: serialized KLIB closure frames, prepared-inline copies and the Native closure ABI.

use crate::common;
use krusty::backend::{Artifact, Backend, CheckedIrFile, CheckedIrUnitKind};
use krusty::diag::DiagSink;
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const LIBRARY: &str = include_str!("fixtures/klib_closure_frames/Library.kt");

const CONSUMER: &str = include_str!("fixtures/klib_closure_frames/Consumer.kt");

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
fn a_dependency_constructor_used_only_inside_nested_closures_is_prepared() {
    let library = include_str!("fixtures/klib_closure_frames/DependencyCalls.kt");
    let consumer = r#"
fun box(): String {
    val f = makeWithConstructor(40)
    return if (f() == 41 && f() == 41) "OK" else "wrong closure result"
}
"#;
    // Imported class storage is an independent Native capability. Observe the actual checked
    // dependency-support IR, with Native's preparation capabilities, before target realization.
    assert_official_dependency_closures(
        library,
        consumer,
        "SerializedClosureDependencies",
        DependencyConsumer::ConstructorIdentity,
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

enum DependencyConsumer {
    Native,
    ConstructorIdentity,
}

/// Observe the same common preparation Native requests without claiming its imported-class
/// storage implementation can execute the construction. Every capability is delegated to Native.
struct ConstructorIdentityProbe {
    native: krusty::native::CraneliftBackend,
    producer_module: String,
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

    fn same_module_inlining(&self) -> krusty::ir::SameModuleInlining {
        self.native.same_module_inlining()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        count: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let expected = krusty::types::type_name("fixture/BodyOnly");
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
            assert_eq!(fact.params, vec![krusty::types::Ty::Int]);
            let identity = fact
                .dependency_callable
                .as_ref()
                .expect("producer-owned constructor identity");
            assert_eq!(identity.module, self.producer_module);
            assert_eq!(
                fact.declaration_signature.as_ref(),
                Some(&identity.declaration)
            );
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
    common::expect_box_ok_with_stdlib(
        &format!("{library_source}\n{consumer}"),
        &format!("{fixture_name}Oracle"),
    );
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
    let consumer_text = format!("package fixture\n{consumer}");
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
        DependencyConsumer::Native => {
            krusty::compiler::emit_analyzed(analysis, &stems, &backend, "consumer", &mut diags)
        }
        DependencyConsumer::ConstructorIdentity => krusty::compiler::emit_analyzed(
            analysis,
            &stems,
            &ConstructorIdentityProbe {
                native: backend,
                producer_module: manifest.unique_name().unwrap().to_owned(),
            },
            "consumer",
            &mut diags,
        ),
    };
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diag| diag.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    if matches!(purpose, DependencyConsumer::ConstructorIdentity) {
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
