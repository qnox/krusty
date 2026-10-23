//! Module-wide in-process compilation helpers.

use krusty::compilation_target::CompilationTarget;
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
    compile(
        sources,
        cp_jars,
        jdk_modules,
        class_major,
        None,
        &krusty::language_settings::LanguageSettings::default(),
    )
}

pub(crate) fn compile(
    sources: &[(&str, &str)],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    class_major: Option<u16>,
    metadata_version: Option<[i32; 3]>,
    language_settings: &krusty::language_settings::LanguageSettings,
) -> Option<Vec<(String, Vec<u8>)>> {
    compile_source_set(
        sources,
        0,
        cp_jars,
        jdk_modules,
        class_major,
        metadata_version,
        language_settings,
    )
}

/// One JVM compilation whose first `common` sources are common sources, as kotlinc's
/// `-Xcommon-sources` names them.
pub(crate) fn compile_source_set(
    sources: &[(&str, &str)],
    common: usize,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
    class_major: Option<u16>,
    metadata_version: Option<[i32; 3]>,
    language_settings: &krusty::language_settings::LanguageSettings,
) -> Option<Vec<(String, Vec<u8>)>> {
    let _pg = super::ProfGuard::new("krusty");
    let mut diags = DiagSink::new();
    let stems = sources
        .iter()
        .map(|(name, _)| {
            name.strip_suffix(".java")
                .or_else(|| name.strip_suffix(".kts"))
                .unwrap_or_else(|| name.trim_end_matches(".kt"))
                .to_string()
        })
        .collect::<Vec<_>>();
    // A `.java` source joins the module as Java and a `.kts` source as a script, the way kotlinc
    // reads a path it is given.
    let inputs = sources
        .iter()
        .zip(&stems)
        .enumerate()
        .map(|(index, ((name, source), stem))| {
            let input = if name.ends_with(".java") {
                SourceInput::java(source)
            } else if name.ends_with(".kts") {
                SourceInput::kotlin_script(source)
            } else {
                SourceInput::kotlin(source)
            }
            .with_file_stem(stem);
            if index < common {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    let cp = super::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone())
            .expect("JVM provider initialization"),
    );
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        super::with_native_plugins(CompilationTarget::Jvm, platform),
        &language_settings.features,
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let backend = krusty::jvm::JvmBackend::new(cp)
        .with_class_major(class_major)
        .with_annotations_in_metadata(language_settings.features.has("AnnotationsInMetadata"))
        .with_pre_release_metadata(language_settings.features.is_pre_release())
        .with_metadata_version(metadata_version);
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

/// The complete frontend report of one multiplatform JVM compilation whose first `common` sources
/// are common sources, each line `file.kt:line:column: severity: message` in emission order, as
/// kotlinc prints it under `-Xmulti-platform -Xcommon-sources=…`.
pub fn source_set_diagnostics(sources: &[(&str, &str)], common: usize) -> Vec<String> {
    let mut features = krusty::features::LangFeatures::new();
    features.enable("MultiPlatformProjects");
    let inputs = sources
        .iter()
        .enumerate()
        .map(|(index, (name, source))| {
            let input = SourceInput::kotlin(source).with_file_stem(name.trim_end_matches(".kt"));
            if index < common {
                input.common()
            } else {
                input
            }
        })
        .collect::<Vec<_>>();
    let jdk = super::jdk_modules();
    let cp = super::cached_classpath(&[super::stdlib_jar()], Some(jdk.as_path()));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        super::with_native_plugins(CompilationTarget::Jvm, platform),
        &features,
        |_, _| {},
        &mut diags,
    );
    let _ = krusty::compiler::check_frontend_only(analysis, &mut diags);
    diags
        .diags
        .iter()
        .map(|diagnostic| {
            let (name, source) = sources[diagnostic.file as usize];
            let (line, column) = krusty::diag::line_col(source, diagnostic.span.lo);
            let severity = match diagnostic.severity {
                krusty::diag::Severity::Error => "error",
                krusty::diag::Severity::Warning => "warning",
            };
            format!("{name}:{line}:{column}: {severity}: {}", diagnostic.msg)
        })
        .collect()
}
