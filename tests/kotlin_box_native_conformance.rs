//! The `codegen/box` corpus through krusty's native pipeline.
//!
//! Every case is a Kotlin source set with `fun box(): String` that returns `"OK"` when the compiler
//! got it right. Its Kotlin files are compiled together by krusty's own code generator with a
//! program entry that prints `box()`'s result, linked by krusty's linker against the prebuilt
//! runtime, and RUN; the executable must print `OK` and exit cleanly. The driver, the verdict and
//! the committed ratchet are every runnable target's (`box_lane`); this file is what is Native's.
//!
//! Multi-file cases use the production source-set path. Multi-module cases are analyzed one source
//! module at a time against generated KLIB dependencies, while one target emission session retains
//! the program backend state; folded `dependsOn` sources remain common inputs rather than being
//! flattened into platform sources.

use std::path::Path;
use std::process::Command;

use krusty::conformance::TestTarget;
use krusty::native::{CraneliftBackend, Entry, NativeTarget};

use super::box_lane::{self, Lane, Libraries, Outcome};
use super::box_ratchet::Platform;

/// Native cases are analyzed against the Kotlin/Native stdlib KLIB, as kotlinc-native compiles them.
const LIBRARIES: Libraries = Libraries::StdlibKlib;

/// The prefix the native backend puts on every decline; what follows names the construct.
const DECLINE_PREFIX: &str = "krusty: the native backend does not support ";

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && LIBRARIES.available(TestTarget::Native)).then_some(target)
}

/// Why a case is outside this harness before any compilation, or `None` when it applies.
fn not_applicable(src: &str) -> Option<&'static str> {
    // A case that NAMES its backends and does not name this one is written for a platform this is
    // not: `// TARGET_BACKEND: JVM` over `object : Runnable` is a program about the JDK, and
    // compiling it here means resolving `java.lang.Runnable` for a target that has no JDK. The
    // widening that admitted them was deliberate and its own comment said when to withdraw it —
    // "when the native target grows its own frontend semantics (klib ingestion, phase 7)" — which
    // is what the klib provider is.
    if !krusty::conformance::backend_targeted(src, &["NATIVE"]) {
        return Some("targeted at another backend");
    }
    // Backend mutes are target-specific. A JVM mute cannot hide a shared-frontend or common-IR
    // failure from the Native lane; that failure belongs in this lane's exact outcome ratchet.
    if krusty::conformance::backend_muted(src, &["NATIVE"]) {
        return Some("muted on the native backend");
    }
    if krusty::conformance::directive(src, "FULL_JDK") {
        return Some("requires the JDK runtime");
    }
    None
}

#[test]
fn native_applicability_uses_native_directives_not_jvm_mutes() {
    assert_eq!(
        not_applicable("// TARGET_BACKEND: NATIVE\n// IGNORE_BACKEND: JVM\nfun box() = \"OK\""),
        None,
        "a JVM-only mute must not remove Native coverage"
    );
    assert_eq!(
        not_applicable("// TARGET_BACKEND: NATIVE\n// IGNORE_BACKEND: NATIVE"),
        Some("muted on the native backend")
    );
    assert_eq!(
        not_applicable("// TARGET_BACKEND: JVM\nfun box() = \"OK\""),
        Some("targeted at another backend")
    );
    assert_eq!(
        not_applicable("// TARGET_BACKEND: ANY\nfun box() = \"OK\""),
        None
    );
    assert_eq!(
        not_applicable("// IGNORE_BACKEND: ANY\nfun box() = \"OK\""),
        Some("muted on the native backend")
    );
    assert_eq!(
        not_applicable("// KJS_WITH_FULL_RUNTIME\n// JVM_TARGET: 1.8\nfun box() = \"OK\""),
        None,
        "another backend's runner options do not remove Native coverage"
    );
    assert_eq!(
        not_applicable("import kotlin.experimental.ExperimentalTypeInference\nfun box() = \"OK\""),
        None,
        "an annotation name is not an unmodeled compiler option"
    );
    assert_eq!(
        not_applicable("// MODULE: lib\nfun answer() = \"OK\""),
        None,
        "a missing harness topology is an expected failure, not an exclusion"
    );
}

/// Why an applicable case cannot yet be constructed by this runner.
fn harness_limitation(src: &str) -> Option<&'static str> {
    // JVM/JS runner switches (`KJS_WITH_FULL_RUNTIME`, `JVM_DEFAULT_MODE`, `LAMBDAS`, …) do not
    // configure a Native compilation. These two options do change this target's semantics, but the
    // missing option model is our failure and therefore stays in the denominator.
    if src
        .lines()
        .any(|line| line.starts_with("// FREE_COMPILER_ARGS:") && line.contains("genericSafeCasts"))
        || krusty::conformance::directive(src, "PROPERTY_LAZY_INITIALIZATION")
    {
        return Some("Native compiler options are not modeled by the harness");
    }
    None
}

#[test]
fn applicable_harness_gaps_are_expected_failures() {
    assert_eq!(
        harness_limitation("// FREE_COMPILER_ARGS: -XgenericSafeCasts\nfun box() = \"OK\""),
        Some("Native compiler options are not modeled by the harness")
    );
}

struct NativeLane {
    target: Option<NativeTarget>,
}

impl NativeLane {
    fn run_artifacts(
        &self,
        scratch: &Path,
        stem: &str,
        suffix: &str,
        program: box_lane::CompiledProgram,
    ) -> Outcome {
        let target = self.target.expect("checked by `unavailable`");
        let objects = program
            .artifacts
            .into_iter()
            .filter_map(|(name, object)| name.ends_with(".o").then_some(object))
            .collect::<Vec<_>>();
        if objects.is_empty() {
            return Outcome::Frontend(
                "the frontend produced no checked file and said nothing".to_string(),
            );
        }
        let object_refs = objects.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let image = match krusty::native::link_program_with_libraries(
            &object_refs,
            &program.native_imports,
            target,
        ) {
            Ok(image) => image,
            Err(error) => return Outcome::Failed(format!("link: {error}")),
        };
        let executable = scratch.join(format!(
            "{stem}-{suffix}-{}-{}",
            std::process::id(),
            rayon::current_thread_index().unwrap_or(0)
        ));
        if let Err(error) = std::fs::write(&executable, &image) {
            return Outcome::Harness(format!("writing the executable: {error}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) =
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            {
                return Outcome::Harness(format!("setting executable permissions: {error}"));
            }
        }
        let outcome = box_lane::run_program(Command::new(&executable).env_clear());
        let _ = std::fs::remove_file(&executable);
        outcome
    }
}

impl Lane for NativeLane {
    fn platform(&self) -> Platform {
        Platform::Native
    }

    fn label(&self) -> &'static str {
        "Native"
    }
    fn env_infix(&self) -> &'static str {
        "NATIVE"
    }

    fn test_target(&self) -> TestTarget {
        TestTarget::Native
    }

    fn libraries(&self) -> Libraries {
        LIBRARIES
    }

    fn unavailable(&self) -> Option<String> {
        self.target.is_none().then(|| {
            "no prebuilt native runtime for this host, or no Kotlin/Native stdlib KLIB".to_string()
        })
    }

    fn not_applicable(&self, src: &str) -> Option<&'static str> {
        not_applicable(src)
    }

    fn harness_limitation(&self, src: &str) -> Option<&'static str> {
        harness_limitation(src)
    }

    fn compile_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        sources: &[(String, String)],
        directive_source: &str,
    ) -> Outcome {
        let target = self.target.expect("checked by `unavailable`");
        let backend = CraneliftBackend::new(target)
            .with_entry(Entry::Box)
            .verified();
        let program = match box_lane::compile(
            sources,
            directive_source,
            LIBRARIES,
            TestTarget::Native,
            &backend,
            "box",
            DECLINE_PREFIX,
        ) {
            Ok(program) => program,
            Err(outcome) => return outcome,
        };
        self.run_artifacts(scratch, stem, "single", program)
    }

    fn compile_modules_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        modules: &[krusty::conformance::ModuleUnit],
        directive_source: &str,
    ) -> Outcome {
        let target = self.target.expect("checked by `unavailable`");
        let backend = CraneliftBackend::new(target)
            .with_entry(Entry::Box)
            .verified();
        let features = krusty::features::LangFeatures::from_source(directive_source);
        let metadata = match krusty::klib::backend::KlibBackend::new(
            krusty::klib::write::KlibPlatform::Native,
            krusty::klib::write::KlibStamp::new(
                krusty::kotlin_version::target(),
                features.language_version(),
            ),
        )
        .with_annotations_in_metadata(features.has("AnnotationsInMetadata"))
        .with_native_target(target)
        {
            Ok(metadata) => metadata,
            Err(error) => return Outcome::Harness(error.to_string()),
        };
        let program = match box_lane::compile_modules(
            scratch,
            stem,
            modules,
            directive_source,
            LIBRARIES,
            TestTarget::Native,
            &backend,
            &metadata,
            DECLINE_PREFIX,
        ) {
            Ok(program) => program,
            Err(outcome) => return outcome,
        };
        self.run_artifacts(scratch, stem, "modules", program)
    }
}

#[test]
fn kotlin_codegen_box_native_conformance() {
    box_lane::run(&NativeLane { target: host() });
}

fn run_native_abi_case(source: &str, stem: &str) -> Outcome {
    if let Some(reason) = not_applicable(source) {
        return Outcome::NotApplicable(reason);
    }
    let Some(target) = host() else {
        return Outcome::Harness("required Native target/KLIB setup is unavailable".into());
    };
    let Some(answer) =
        super::kotlin_native_multimodule_support::kotlin_native_multimodule_box(source)
    else {
        return Outcome::Harness(
            "required pinned kotlinc-native module oracle is unavailable".into(),
        );
    };
    assert_eq!(
        answer, "OK",
        "{}: pinned kotlinc-native must run the exact KLIB module graph as OK",
        stem
    );
    let modules = krusty::conformance::split_modules(source).expect("valid module test shape");
    let units = krusty::conformance::module_units(&modules);
    let lane = NativeLane {
        target: Some(target),
    };
    lane.compile_modules_and_run(
        &std::env::temp_dir(),
        &format!("native-{stem}"),
        &units,
        source,
    )
}

/// A required inline dependency function whose body cannot become a template rejects its caller:
/// it is never compiled as a direct call of the dependency function.
// PLATFORM-SPECIFIC: Native expands required KLIB inline bodies in common IR; JVM uses bytecode.
#[test]
fn native_a_declined_required_inline_body_rejects_the_caller() {
    let case = super::dependency_template_rejections::DECLINED_REQUIRED_INLINE_BODY;
    super::box_reference_classes::checked_box(case.source, case.stem)
        .expect("pinned kotlinc must run the required-inline rejection fixture");
    let Some(target) = host() else {
        if std::env::var_os("KRUSTY_REQUIRE_NATIVE_CONFORMANCE").is_some() {
            panic!(
                "{}: required Native target/KLIB setup is unavailable",
                case.stem
            );
        }
        return;
    };
    let modules = krusty::conformance::split_modules(case.source).expect("valid module test shape");
    let units = krusty::conformance::module_units(&modules);
    let lane = NativeLane {
        target: Some(target),
    };
    let outcome =
        lane.compile_modules_and_run(&std::env::temp_dir(), case.stem, &units, case.source);
    assert!(
        matches!(&outcome, Outcome::Frontend(message)
            if super::dependency_template_rejections::required_inline_rejection(message).is_some()),
        "{}: {outcome:?}",
        case.stem
    );
}

/// A required stdlib inline function whose template declines rejects its caller.
// PLATFORM-SPECIFIC: this tests the Native KLIB template-decline contract, not Kotlin behavior.
#[test]
fn native_a_declined_required_stdlib_template_rejects_the_caller() {
    super::dependency_template_rejections::verify_declined_stdlib_source_with_kotlinc();
    let src = super::dependency_template_rejections::DECLINED_STDLIB_INLINE_BODY;
    let stem = "declinedRequiredStdlibTemplate";
    let Some(target) = host() else {
        if std::env::var_os("KRUSTY_REQUIRE_NATIVE_CONFORMANCE").is_some() {
            panic!("{stem}: required Native target/KLIB setup is unavailable");
        }
        return;
    };
    let lane = NativeLane {
        target: Some(target),
    };
    let outcome = lane.compile_and_run(
        &std::env::temp_dir(),
        stem,
        &[(format!("{stem}.kt"), src.to_string())],
        src,
    );
    assert!(
        matches!(&outcome, Outcome::Frontend(message)
            if super::dependency_template_rejections::required_inline_rejection(message).is_some()),
        "{stem}: {outcome:?}"
    );
}

#[test]
fn native_module_abi_symbols_keep_same_named_internal_classifiers_distinct() {
    // PLATFORM-SPECIFIC: Native value-class syntax and module-qualified runtime/box/unbox symbols.
    let source = "// TARGET_BACKEND: NATIVE\n\
                 // MODULE: left\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
                 // MODULE: right\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
                 // MODULE: main(left, right)\nfun box() = \"OK\"\n";
    assert_eq!(
        run_native_abi_case(source, "multimoduleNativeAbiSymbols"),
        Outcome::Pass,
        "Native ABI identities must remain distinct across the exact module graph"
    );
}
