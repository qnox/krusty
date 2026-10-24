//! Reference-compiler `box()` execution shared by full and focused end-to-end targets.

use std::path::PathBuf;

use super::common_core as common;
use super::reference_compiler_support::{kotlinc_paths_result, write_fixture_sources};

/// Compile one `Main.kt` fixture with kotlinc and run its `box()` result on the shared JVM.
pub fn kotlinc_box_result(source: &str) -> String {
    kotlinc_box_result_with_classpath(source, &[])
}

/// Compile one `Main.kt` fixture with kotlinc against caller-supplied dependencies and run its
/// `box()` result on the shared JVM. The fixture's `// LANGUAGE:` directives become kotlinc flags.
pub fn kotlinc_box_result_with_classpath(source: &str, classpath: &[PathBuf]) -> String {
    let work = common::scratch_dir().expect("cannot allocate reference-runtime fixture");
    let source_paths = write_fixture_sources(&work, &[("Main.kt", source)]);
    let output = work.join("out");
    let mut extra_args = common::language_directives::kotlinc_args(source);
    if !classpath.is_empty() {
        extra_args.push("-cp".to_string());
        extra_args.push(
            std::env::join_paths(classpath)
                .expect("build reference-runtime classpath")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let (code, diagnostics) = kotlinc_paths_result(&source_paths, &output, &extra_args);
    assert_eq!(code, 0, "kotlinc rejected runtime fixture: {diagnostics}");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let mut runtime_classpath = vec![output];
    runtime_classpath.extend_from_slice(classpath);
    runtime_classpath.extend([stdlib, jdk]);
    let result =
        common::run_box(&[], "MainKt", &runtime_classpath).expect("run kotlinc-built box fixture");
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Compile named fixtures with kotlinc against caller-supplied runtime dependencies and run
/// `main_class`'s `box()` on the shared JVM.
pub fn kotlinc_box_files_result_with_classpath(
    sources: &[(&str, &str)],
    main_class: &str,
    classpath: &[PathBuf],
) -> String {
    let work = common::scratch_dir().expect("cannot allocate reference-runtime fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let output = work.join("out");
    let mut extra_args = Vec::new();
    if !classpath.is_empty() {
        extra_args.push("-cp".to_string());
        extra_args.push(
            std::env::join_paths(classpath)
                .expect("build reference-runtime classpath")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let (code, diagnostics) = kotlinc_paths_result(&source_paths, &output, &extra_args);
    assert_eq!(code, 0, "kotlinc rejected runtime fixture: {diagnostics}");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let mut runtime_classpath = vec![output];
    runtime_classpath.extend_from_slice(classpath);
    runtime_classpath.extend([stdlib, jdk]);
    let result = common::run_box(&[], main_class, &runtime_classpath)
        .expect("run kotlinc-built multi-file box fixture");
    let _ = std::fs::remove_dir_all(work);
    result
}
