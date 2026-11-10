//! Classpath and JDK chosen for one refreshed project model, and the log lines that describe them.

use std::path::{Path, PathBuf};

use krusty_lsp::{
    resolve_jdk, JdkRequest, LspOptions, ProcessRunner, ProjectSync, ProviderKind,
    SystemEnvironment,
};

pub(super) fn describe_model(
    kind: ProviderKind,
    modules: usize,
    classpath: &[PathBuf],
    jdk_home: Option<&Path>,
) -> Vec<String> {
    const MAX_LISTED: usize = 60;
    let mut logs = vec![format!(
        "krusty: {} — {modules} module(s), {} classpath entr{}",
        kind.as_str(),
        classpath.len(),
        if classpath.len() == 1 { "y" } else { "ies" },
    )];
    logs.push(format!(
        "krusty: JDK = {}",
        jdk_home.map_or_else(|| "none".to_string(), |home| home.display().to_string()),
    ));
    if !classpath.is_empty() {
        let mut listing = String::from("krusty: classpath:");
        for entry in classpath.iter().take(MAX_LISTED) {
            listing.push_str("\n  ");
            listing.push_str(&entry.to_string_lossy());
        }
        if classpath.len() > MAX_LISTED {
            listing.push_str(&format!("\n  … {} more", classpath.len() - MAX_LISTED));
        }
        logs.push(listing);
    }
    logs
}

pub(super) fn launch_from(
    sync: &ProjectSync,
    options: &LspOptions,
    runner: &ProcessRunner,
) -> (Vec<PathBuf>, Option<PathBuf>) {
    let classpath = sync.project_classpath();
    if options.no_jdk() {
        return (classpath, None);
    }
    let toolchain = sync.model().and_then(|model| model.jdk_home.clone());
    let jdk = resolve_jdk(
        &SystemEnvironment,
        runner,
        &JdkRequest {
            explicit: options.jdk_home(),
            toolchain: toolchain.as_deref(),
            jvm_target: sync.jvm_target(),
        },
    );
    (classpath, jdk.map(|jdk| jdk.home))
}
