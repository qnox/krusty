//! Which project a `krusty-toolchain build` invocation is standing in.
//!
//! The Kotlin Toolchain layout (`module.yaml` / `project.yaml`) is the project this command
//! compiles. Gradle, Maven, Bazel, and a JetBrains `.iml` model are recognized only so they are
//! not read as that layout. Gradle, Maven, and `.iml` dependencies are read by the language
//! server. A Gradle, Maven, or Bazel build compiles through its own plugin.

use std::path::{Path, PathBuf};

const MAX_ANCESTORS: usize = 16;

const GRADLE_MARKERS: &[&str] = &[
    "settings.gradle",
    "settings.gradle.kts",
    "build.gradle",
    "build.gradle.kts",
    "gradlew",
    "gradlew.bat",
];

const MAVEN_MARKERS: &[&str] = &["pom.xml", "mvnw", "mvnw.cmd"];

const BAZEL_MARKERS: &[&str] = &[
    "MODULE.bazel",
    "WORKSPACE",
    "WORKSPACE.bazel",
    "BUILD.bazel",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProjectKind {
    Toolchain(PathBuf),
    Gradle(PathBuf),
    Maven(PathBuf),
    Bazel(PathBuf),
    Jps(PathBuf),
}

pub(super) fn discover(start: &Path) -> Option<ProjectKind> {
    let mut project = None;
    let mut module = None;
    let mut jps = None;
    for directory in start.ancestors().take(MAX_ANCESTORS) {
        let project_file = directory.join("project.yaml").is_file();
        let module_file = directory.join("module.yaml").is_file();
        if project_file || module_file {
            if project_file {
                // Walking upward, so the last hit is the outermost project.
                project = Some(directory.to_path_buf());
            }
            if module.is_none() && module_file {
                module = Some(directory.to_path_buf());
            }
            continue;
        }
        if project.is_some() || module.is_some() {
            if jps.is_none() && directory.join(".idea").join("modules.xml").is_file() {
                jps = Some(ProjectKind::Jps(directory.to_path_buf()));
            }
            continue;
        }
        if has_marker(directory, GRADLE_MARKERS) {
            return Some(ProjectKind::Gradle(directory.to_path_buf()));
        }
        if has_marker(directory, MAVEN_MARKERS) {
            return Some(ProjectKind::Maven(directory.to_path_buf()));
        }
        if has_marker(directory, BAZEL_MARKERS) {
            return Some(ProjectKind::Bazel(directory.to_path_buf()));
        }
        if jps.is_none() && directory.join(".idea").join("modules.xml").is_file() {
            jps = Some(ProjectKind::Jps(directory.to_path_buf()));
        }
    }
    project
        .map(ProjectKind::Toolchain)
        .or_else(|| module.map(ProjectKind::Toolchain))
        .or(jps)
}

pub(super) fn extension_message(kind: &ProjectKind) -> Option<String> {
    match kind {
        ProjectKind::Toolchain(_) => None,
        ProjectKind::Gradle(root) => Some(plugin_message(root, "Gradle", "Gradle plugin")),
        ProjectKind::Maven(root) => Some(plugin_message(root, "Maven", "Maven plugin")),
        ProjectKind::Bazel(root) => Some(plugin_message(root, "Bazel", "Bazel plugin")),
        ProjectKind::Jps(root) => Some(format!(
            "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
             {} is a JetBrains .iml project. .iml dependencies are read by the language server, not by this command. Describe the modules in module.yaml to compile them.",
            root.display()
        )),
    }
}

fn plugin_message(root: &Path, kind: &str, plugin: &str) -> String {
    format!(
        "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
         {} is a {kind} project. Compile it with the {plugin}, or describe the modules in module.yaml. This command does not read {kind} projects.",
        root.display()
    )
}

fn has_marker(directory: &Path, markers: &[&str]) -> bool {
    markers
        .iter()
        .any(|marker| directory.join(marker).is_file())
}
