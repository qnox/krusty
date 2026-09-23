//! A generated `$serializer` referenced from ANOTHER FILE of the same module must still get an
//! `InnerClasses` row.
//!
//! The serialization plugin publishes the exact generated classifier with its source header, so a
//! sibling file can consume the same semantic declaration fact before that owner's IR is lowered.
//! This prevents the JVM backend from recognizing the generated class by its rendered suffix.
//!
//! The comparison is against kotlinc's own table rather than a hand-written expectation: the row's
//! `access_flags` carry `ACC_SYNTHETIC`, which `javap` does not print on an `InnerClasses` line, so
//! an eyeballed expectation would have accepted the defective 0x0019.
//!
//! A whole-class byte differential cannot be used here — the in-process compilation path these
//! helpers take emits `@Metadata` `d1` function flags the shipped CLI does not, for exactly the
//! plugin-generated members this fixture creates.

use std::path::PathBuf;
use std::sync::OnceLock;

use krusty::jvm::classreader::{parse_class, InnerClassRef};

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

/// The two source files, as `(stem, source)` — one `@Serializable` class per file, the second
/// holding the first both directly and as a nullable property.
fn fixture() -> [(&'static str, &'static str); 2] {
    [
        (
            "Leafy",
            "import kotlinx.serialization.Serializable\n\
             @Serializable\n\
             data class Leafy(val id: Int)\n",
        ),
        (
            "Branchy",
            "import kotlinx.serialization.Serializable\n\
             @Serializable\n\
             data class Branchy(val leafy: Leafy, val maybe: Leafy?)\n",
        ),
    ]
}

/// kotlinc's own class files for the fixture, compiled with ITS serialization plugin.
fn reference_classes(stem: &str) -> PathBuf {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-reference"));
    std::fs::create_dir_all(&work).expect("create reference fixture directory");
    let mut sources = Vec::new();
    for (name, source) in fixture() {
        let path = work.join(format!("{name}.kt"));
        std::fs::write(&path, source).expect("write reference fixture");
        sources.push(path.display().to_string());
    }
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{stem}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "{stem}: the reference serialization plugin is missing at {}",
        plugin.display()
    );
    let joined = std::env::join_paths(runtime_jars()).expect("join the reference classpath");
    let mut arguments = vec![
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        joined.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
    ];
    arguments.extend(sources);
    let (code, diagnostics) = common::kotlinc_compile(&arguments)
        .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the fixture: {diagnostics}"
    );
    out
}

fn inner_classes(bytes: &[u8], side: &str, class: &str) -> Vec<InnerClassRef> {
    parse_class(bytes)
        .unwrap_or_else(|error| panic!("{side} {class} is unreadable: {error:?}"))
        .inner_classes
}

/// Both `Branchy` and `Branchy$$serializer` name `Leafy$$serializer` as a class constant — the
/// serialized class through its child-serializer factory, the serializer through `childSerializers`
/// — so each must carry a row for it.
#[test]
fn a_cross_file_generated_serializer_gets_the_inner_classes_row_kotlinc_writes() {
    let stem = "cross_file_serializer_inner_classes";
    let reference_dir = reference_classes(stem);
    let classes = common::compile_in_process_files(
        &fixture()
            .iter()
            .map(|&(name, source)| (name, source))
            .collect::<Vec<_>>(),
        &runtime_jars(),
        Some(common::jdk_modules().as_path()),
    )
    .unwrap_or_else(|| panic!("{stem}: krusty failed to compile the fixture"));

    for class in ["Branchy", "Branchy$$serializer"] {
        let reference = std::fs::read(reference_dir.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("{stem}: kotlinc emitted no {class}"));
        let (_, actual) = classes
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| {
                let emitted: Vec<&String> = classes.iter().map(|(name, _)| name).collect();
                panic!("{stem}: krusty emitted no {class}; emitted: {emitted:?}")
            });
        let expected = inner_classes(&reference, "kotlinc", class);
        assert!(
            expected
                .iter()
                .any(|entry| entry.inner == "Leafy$$serializer"),
            "{stem}: the fixture no longer reproduces a cross-file reference — kotlinc's {class} \
             has no Leafy$$serializer row: {expected:?}"
        );
        assert_eq!(
            inner_classes(actual, "krusty", class),
            expected,
            "{stem}: {class}'s InnerClasses table disagrees with kotlinc"
        );
    }
}
