//! Module-wide in-process compilation helpers.

use std::path::PathBuf;

use krusty::diag::DiagSink;
use krusty::source::SourceInput;

pub fn compile_in_process_files(
    sources: &[(&str, &str)],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    compile_in_process_files_target(sources, cp_jars, jdk_modules, None)
}

/// [`compile_in_process_files`] emitting class files of major version `class_major` (`None` keeps
/// the backend's default), for a byte comparison with kotlinc's `-jvm-target` output.
pub fn compile_in_process_files_target(
    sources: &[(&str, &str)],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    class_major: Option<u16>,
) -> Option<Vec<(String, Vec<u8>)>> {
    let _pg = super::ProfGuard::new("krusty");
    let mut diags = DiagSink::new();
    let stems = sources
        .iter()
        .map(|(name, _)| name.trim_end_matches(".kt").to_string())
        .collect::<Vec<_>>();
    let inputs = sources
        .iter()
        .zip(&stems)
        .map(|((_, source), stem)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let cp = super::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone())
            .expect("JVM provider initialization"),
    );
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        super::with_native_plugins(platform),
        &krusty::features::LangFeatures::default(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let backend = krusty::jvm::JvmBackend::new(cp).with_class_major(class_major);
    let outputs = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    let classes = outputs
        .into_iter()
        .map(|(path, bytes)| {
            (
                path.strip_suffix(".class").unwrap_or(&path).to_string(),
                bytes,
            )
        })
        .collect::<Vec<_>>();
    (!diags.has_errors() && !classes.is_empty()).then_some(classes)
}
