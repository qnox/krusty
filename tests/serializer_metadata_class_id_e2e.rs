//! A generated `$serializer`'s `@Metadata` must name a class id that decodes back to that class.
//!
//! The serialization plugin generates a nested class literally named `$serializer`, so the JVM
//! class is `Owner$$serializer`. krusty recorded its own name in `d2[0]` the way it records every
//! other class id — as a descriptor carrying the string table's `DESC_TO_CLASS_ID` operation:
//!
//! ```text
//! krusty : Lpkg/Owner$$serializer;
//! kotlinc: pkg/Owner.$serializer
//! ```
//!
//! That operation strips `L`/`;` and replaces EVERY `$` with `.`, so krusty's form decodes to
//! `pkg/Owner..serializer` — a class id with a doubled separator that names nothing. The descriptor
//! encoding simply cannot express a nested class whose own simple name begins with `$`; kotlinc
//! writes the class id directly for exactly that reason.
//!
//! This is the single most systematic byte difference measured across the generated-client corpus:
//! every generated serializer carries it.

use std::path::PathBuf;
use std::sync::OnceLock;

use super::common;

const SERIALIZATION_VERSION: &str = "1.9.0";

fn provisioned(artifact: &str) -> PathBuf {
    krusty::toolchain::ensure_maven("org.jetbrains.kotlinx", artifact, SERIALIZATION_VERSION)
        .unwrap_or_else(|| {
            panic!(
                "could not provision {artifact}:{SERIALIZATION_VERSION}; this test must not \
                 self-skip, since a skipped serialization test passes on a compiler that would \
                 have rejected the fixture. Check network access or set KRUSTY_DEPS_CACHE."
            )
        })
}

fn runtime_jars() -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        vec![
            common::stdlib_jar(),
            provisioned("kotlinx-serialization-core-jvm"),
        ]
    })
    .clone()
}

/// kotlinc's own class files for `src`, compiled with ITS serialization plugin.
fn reference_classes(src: &str, stem: &str) -> PathBuf {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-reference"));
    std::fs::create_dir_all(&work).expect("create reference fixture directory");
    let source = work.join("Main.kt");
    std::fs::write(&source, src).expect("write reference fixture");
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{stem}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "{stem}: the reference serialization plugin is missing at {}",
        plugin.display()
    );
    let joined = std::env::join_paths(&runtime_jars()).expect("join the reference classpath");
    let (code, diagnostics) = common::kotlinc_compile(&[
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the fixture: {diagnostics}"
    );
    out
}

/// Compare the `@Metadata` `d2` string table of one class against kotlinc's.
///
/// `d2` is where a class records its own id and its members' names, so it is the half that carries
/// this defect; `d1` is compared separately by the serializer-shape work and still differs for
/// unrelated reasons.
fn assert_d2_matches_kotlinc(src: &str, stem: &str, class_internal: &str) {
    let reference_dir = reference_classes(src, stem);
    let reference = std::fs::read(reference_dir.join(format!("{class_internal}.class")))
        .unwrap_or_else(|_| panic!("{stem}: kotlinc emitted no {class_internal}"));
    let classes = common::compile_in_process(src, stem, &runtime_jars(), None)
        .unwrap_or_else(|| panic!("{stem}: krusty failed to compile the fixture"));
    let (_, actual) = classes
        .iter()
        .find(|(name, _)| name == class_internal)
        .unwrap_or_else(|| panic!("{stem}: krusty emitted no {class_internal}"));

    let (_, reference_d2) =
        super::serializer_metadata_test_support::raw_kotlin_metadata(&reference)
            .unwrap_or_else(|| panic!("{stem}: kotlinc's {class_internal} carries no @Metadata"));
    let (_, actual_d2) = super::serializer_metadata_test_support::raw_kotlin_metadata(actual)
        .unwrap_or_else(|| panic!("{stem}: krusty's {class_internal} carries no @Metadata"));
    let reference_class_id = reference_d2
        .first()
        .unwrap_or_else(|| panic!("{stem}: kotlinc metadata has no class-id string"));
    let actual_class_id = actual_d2
        .first()
        .unwrap_or_else(|| panic!("{stem}: krusty metadata has no class-id string"));

    assert_eq!(
        actual_class_id, reference_class_id,
        "{stem}: {class_internal} records a different class id than kotlinc"
    );
}

const SRC: &str = "import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
data class Retention(val days: Int)\n";

/// The generated serializer records the class id kotlinc records.
#[test]
fn a_generated_serializer_records_kotlincs_class_id() {
    assert_d2_matches_kotlinc(SRC, "serializer_class_id", "Retention$$serializer");
}

/// The `@Serializable` class itself is the control: its own name has no `$`, so the descriptor
/// encoding round-trips and both compilers already agree. It must keep agreeing.
#[test]
fn the_serializable_class_itself_still_records_its_own_class_id() {
    assert_d2_matches_kotlinc(SRC, "serializable_class_id", "Retention");
}
