//! The `codegen/box` corpus through krusty's native pipeline.
//!
//! Every case is a Kotlin source set with `fun box(): String` that returns `"OK"` when the compiler
//! got it right. Its Kotlin files are compiled together by krusty's own code generator with a
//! program entry that prints `box()`'s result, linked by krusty's linker against the prebuilt
//! runtime, and RUN; the executable must print `OK` and exit cleanly. The driver, the verdict and
//! the committed ratchet are every runnable target's (`box_lane`); this file is what is Native's.
//!
//! Multi-file cases use the production source-set path. Multi-module cases stay applicable and are
//! expected failures until their dependency edges are wired to the module provider; flattening them
//! would test a different program.

use std::path::Path;
use std::process::Command;

use krusty::conformance::TestTarget;
use krusty::native::{CraneliftBackend, Entry, NativeTarget};

use super::box_lane::{self, Lane, Outcome};
use super::box_ratchet::Platform;

/// The prefix the native backend puts on every decline; what follows names the construct.
const DECLINE_PREFIX: &str = "krusty: the native backend does not support ";

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && box_lane::frontend_available()).then_some(target)
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
    if src.contains("// MODULE:") {
        return Some("multi-module compilation is not wired to the module dependency provider");
    }
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
        harness_limitation("// MODULE: lib\nfun answer() = \"OK\""),
        Some("multi-module compilation is not wired to the module dependency provider")
    );
    assert_eq!(
        harness_limitation("// FREE_COMPILER_ARGS: -XgenericSafeCasts\nfun box() = \"OK\""),
        Some("Native compiler options are not modeled by the harness")
    );
}

struct NativeLane {
    target: Option<NativeTarget>,
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

    fn unavailable(&self) -> Option<String> {
        self.target.is_none().then(|| {
            "no prebuilt native runtime for this host, or no stdlib and JDK modules".to_string()
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
        let artifacts = match box_lane::compile(
            sources,
            directive_source,
            TestTarget::Native,
            &backend,
            "box",
            DECLINE_PREFIX,
        ) {
            Ok(artifacts) => artifacts,
            Err(outcome) => return outcome,
        };
        let objects = artifacts
            .into_iter()
            .filter_map(|(name, object)| name.ends_with(".o").then_some(object))
            .collect::<Vec<_>>();
        if objects.is_empty() {
            // The frontend produced no checked file for the backend to lower and said nothing
            // about it. That is the JVM pipeline's silence to account for, not a native miscompile.
            return Outcome::Frontend(
                "the frontend produced no checked file and said nothing".to_string(),
            );
        }
        let object_refs = objects.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let image = match krusty::native::link_program(&object_refs, target) {
            Ok(image) => image,
            Err(error) => return Outcome::Failed(format!("link: {error}")),
        };
        let executable = scratch.join(format!(
            "{stem}-{}-{}",
            std::process::id(),
            rayon::current_thread_index().unwrap_or(0)
        ));
        if let Err(error) = std::fs::write(&executable, &image) {
            return Outcome::Failed(format!("writing the executable: {error}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
        }
        let outcome = box_lane::run_program(Command::new(&executable).env_clear());
        let _ = std::fs::remove_file(&executable);
        outcome
    }
}

#[test]
fn kotlin_codegen_box_native_conformance() {
    box_lane::run(&NativeLane { target: host() });
}
