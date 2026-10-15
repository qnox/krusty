//! Located front-end diagnostics under an explicitly selected compilation target.

use std::path::{Path, PathBuf};

use super::common_core as common;

/// Complete front-end diagnostic records, in emission order, under the explicitly selected
/// target's source rules.
pub fn front_end_diagnostic_records_for(
    target: krusty::compilation_target::CompilationTarget,
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> Vec<krusty::diag::Diagnostic> {
    front_end_diagnostic_records_with_features_for(
        target,
        src,
        cp_jars,
        jdk_modules,
        &krusty::features::LangFeatures::new(),
    )
}

/// Complete front-end diagnostic records under an explicitly selected target and release-owned
/// language feature set.
pub fn front_end_diagnostic_records_with_features_for(
    target: krusty::compilation_target::CompilationTarget,
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
    features: &krusty::features::LangFeatures,
) -> Vec<krusty::diag::Diagnostic> {
    let cp = common::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let inputs = [krusty::source::SourceInput::kotlin(src)];
    let mut diags = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        common::with_native_plugins(target, platform),
        features,
        |_, _| {},
        &mut diags,
    );
    let _ = krusty::compiler::check_frontend_only(analysis, &mut diags);
    diags.diags
}

/// Front-end diagnostics rendered as `line:col: severity: message`, in emission order, under the
/// explicitly selected target's source rules.
pub fn front_end_diagnostics_located_for(
    target: krusty::compilation_target::CompilationTarget,
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> Vec<String> {
    front_end_diagnostic_records_for(target, src, cp_jars, jdk_modules)
        .iter()
        .map(|diagnostic| {
            let (line, column) = krusty::diag::line_col(src, diagnostic.span.lo);
            let severity = match diagnostic.severity {
                krusty::diag::Severity::Error => "error",
                _ => "warning",
            };
            format!("{line}:{column}: {severity}: {}", diagnostic.msg)
        })
        .collect()
}
