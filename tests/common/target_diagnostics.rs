//! Located front-end diagnostics under an explicitly selected compilation target.

use std::path::{Path, PathBuf};

use super::common_core as common;

/// Front-end diagnostics rendered as `line:col: severity: message`, in emission order, under the
/// explicitly selected target's source rules.
pub fn front_end_diagnostics_located_for(
    target: krusty::compilation_target::CompilationTarget,
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&Path>,
) -> Vec<String> {
    let cp = common::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let inputs = [krusty::source::SourceInput::kotlin(src)];
    let mut diags = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        common::with_native_plugins(target, platform),
        &krusty::features::LangFeatures::new(),
        |_, _| {},
        &mut diags,
    );
    let _ = krusty::compiler::check_frontend_only(analysis, &mut diags);
    diags
        .diags
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
