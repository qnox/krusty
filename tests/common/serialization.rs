//! Shared support for serialization-plugin runtime differentials.

use std::path::PathBuf;
use std::sync::OnceLock;

use super::common;

const SERIALIZATION_VERSION: &str = "1.9.0";

fn provisioned(artifact: &str) -> PathBuf {
    krusty::toolchain::ensure_maven(
        "org.jetbrains.kotlinx",
        artifact,
        SERIALIZATION_VERSION,
    )
    .unwrap_or_else(|| {
        panic!(
            "could not provision {artifact}:{SERIALIZATION_VERSION}; serialization differentials \n+             must not self-skip. Check network access or set KRUSTY_DEPS_CACHE."
        )
    })
}

fn runtime_jars() -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        vec![
            common::stdlib_jar(),
            provisioned("kotlinx-serialization-core-jvm"),
            provisioned("kotlinx-serialization-json-jvm"),
        ]
    })
    .clone()
}

fn reference_box(src: &str, stem: &str) -> String {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-serialization-reference"));
    std::fs::create_dir_all(&work).expect("create serialization reference fixture directory");
    let source = work.join("Main.kt");
    std::fs::write(&source, src).expect("write serialization reference fixture");
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{stem}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "{stem}: the reference serialization plugin is missing at {}",
        plugin.display()
    );
    let jars = runtime_jars();
    let classpath = std::env::join_paths(&jars).expect("join the serialization classpath");
    let (code, diagnostics) = common::kotlinc_compile(&[
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-opt-in=kotlin.time.ExperimentalTime,kotlin.uuid.ExperimentalUuidApi".to_string(),
        "-cp".to_string(),
        classpath.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the fixture: {diagnostics}"
    );
    let mut classpath = vec![out];
    classpath.extend(jars);
    common::run_box(&[], "MainKt", &classpath)
        .unwrap_or_else(|| panic!("{stem}: the reference-built box() did not run"))
}

fn krusty_box(src: &str, stem: &str) -> String {
    let jars = runtime_jars();
    let classes = common::compile_in_process(src, stem, &jars, None).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(src, &jars, None);
        let outcome = common::backend_outcome_in_process(src, stem, &jars, None);
        panic!(
            "krusty failed to compile {stem}; diagnostics: {diagnostics:?}; backend: {outcome:?}"
        )
    });
    let box_class =
        common::find_box_class(&classes).unwrap_or_else(|| panic!("no box class for {stem}"));
    common::run_box(&classes, &box_class, &jars)
        .unwrap_or_else(|| panic!("box() did not run for {stem}"))
}

/// Run the identical serialization fixture under both compilers and require the exact same result.
pub(super) fn both_compilers_box(src: &str, stem: &str) -> String {
    let reference = reference_box(src, stem);
    let actual = krusty_box(src, stem);
    assert_eq!(actual, reference, "{stem}: krusty disagrees with kotlinc");
    actual
}
