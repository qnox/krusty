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

fn reference_box_files(sources: &[(&str, &str)], stem: &str) -> String {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-serialization-reference-files"));
    std::fs::create_dir_all(&work).expect("create serialization reference fixture directory");
    let mut written = Vec::new();
    for (name, src) in sources {
        let source = work.join(name);
        std::fs::write(&source, src).expect("write serialization reference fixture");
        written.push(source);
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
    let jars = runtime_jars();
    let classpath = std::env::join_paths(&jars).expect("join the serialization classpath");
    let mut args = vec![
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        classpath.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
    ];
    args.extend(written.iter().map(|path| path.display().to_string()));
    let (code, diagnostics) = common::kotlinc_compile(&args)
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

fn krusty_box_files(sources: &[(&str, &str)], stem: &str) -> String {
    let jars = runtime_jars();
    let classes = common::compile_in_process_files(sources, &jars, None)
        .unwrap_or_else(|| panic!("krusty failed to compile the {stem} fixture"));
    let box_class =
        common::find_box_class(&classes).unwrap_or_else(|| panic!("no box class for {stem}"));
    common::run_box(&classes, &box_class, &jars)
        .unwrap_or_else(|| panic!("box() did not run for {stem}"))
}

/// Compile `lib_src` as a separate module with the reference compiler and its serialization
/// plugin, the way a published dependency reaches a consumer.
pub(super) fn reference_dependency(lib_src: &str, stem: &str) -> PathBuf {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-serialization-dependency"));
    std::fs::create_dir_all(&work).expect("create serialization dependency directory");
    let source = work.join("Lib.kt");
    std::fs::write(&source, lib_src).expect("write serialization dependency source");
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{stem}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    let classpath = std::env::join_paths(runtime_jars()).expect("join the serialization classpath");
    let (code, diagnostics) = common::kotlinc_compile(&[
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        classpath.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the dependency: {diagnostics}"
    );
    out
}

/// [`both_compilers_box`] for a consumer of a separately compiled dependency: `lib_src` is built
/// once by the reference compiler, and each compiler builds `src` against those class files.
pub(super) fn both_compilers_box_against_dependency(
    lib_src: &str,
    src: &str,
    stem: &str,
) -> String {
    let dependency = reference_dependency(lib_src, stem);
    let mut jars = vec![dependency];
    jars.extend(runtime_jars());

    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{stem}: cannot allocate a scratch directory"))
        .join(format!("{stem}-serialization-consumer"));
    std::fs::create_dir_all(&work).expect("create serialization consumer directory");
    let source = work.join("Main.kt");
    std::fs::write(&source, src).expect("write serialization consumer source");
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()
        .unwrap_or_else(|| panic!("{stem}: no reference compiler lib directory"))
        .join("kotlinx-serialization-compiler-plugin.jar");
    let classpath = std::env::join_paths(&jars).expect("join the consumer classpath");
    let (code, diagnostics) = common::kotlinc_compile(&[
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-cp".to_string(),
        classpath.to_string_lossy().into_owned(),
        "-d".to_string(),
        out.display().to_string(),
        source.display().to_string(),
    ])
    .unwrap_or_else(|| panic!("{stem}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{stem}: kotlinc rejected the consumer: {diagnostics}"
    );
    let mut reference_classpath = vec![out];
    reference_classpath.extend(jars.iter().cloned());
    let reference = common::run_box(&[], "MainKt", &reference_classpath)
        .unwrap_or_else(|| panic!("{stem}: the reference-built box() did not run"));

    let classes = common::compile_in_process(src, stem, &jars, None).unwrap_or_else(|| {
        let diagnostics = common::front_end_diagnostics(src, &jars, None);
        let outcome = common::backend_outcome_in_process(src, stem, &jars, None);
        panic!(
            "krusty failed to compile {stem}; diagnostics: {diagnostics:?}; backend: {outcome:?}"
        )
    });
    let box_class =
        common::find_box_class(&classes).unwrap_or_else(|| panic!("no box class for {stem}"));
    let actual = common::run_box(&classes, &box_class, &jars)
        .unwrap_or_else(|| panic!("box() did not run for {stem}"));
    assert_eq!(actual, reference, "{stem}: krusty disagrees with kotlinc");
    actual
}

/// The multi-file form of [`both_compilers_box`]: the file split is itself the discriminator for a
/// serializer a sibling file declares.
pub(super) fn both_compilers_box_files(sources: &[(&str, &str)], stem: &str) -> String {
    let reference = reference_box_files(sources, stem);
    let actual = krusty_box_files(sources, stem);
    assert_eq!(actual, reference, "{stem}: krusty disagrees with kotlinc");
    actual
}

/// Run the identical serialization fixture under both compilers and require the exact same result.
pub(super) fn both_compilers_box(src: &str, stem: &str) -> String {
    let reference = reference_box(src, stem);
    let actual = krusty_box(src, stem);
    assert_eq!(actual, reference, "{stem}: krusty disagrees with kotlinc");
    actual
}
