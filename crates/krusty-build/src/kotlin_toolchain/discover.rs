//! Which project a `krusty-toolchain build` invocation is standing in.
//!
//! A `project.yaml` owns every `module.yaml` beneath it. Gradle and Maven are located by their
//! markers and then compiled by running those tools; the marker file is not read. A JetBrains
//! `.iml` model is recognized so it is not mistaken for a toolchain project, and compiling it
//! stays with that project-model extension.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProjectKind {
    Toolchain(PathBuf),
    Gradle(PathBuf),
    Maven(PathBuf),
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
        if project.is_none() && module.is_none() && has_marker(directory, GRADLE_MARKERS) {
            return Some(ProjectKind::Gradle(directory.to_path_buf()));
        }
        if project.is_none() && module.is_none() && has_marker(directory, MAVEN_MARKERS) {
            return Some(ProjectKind::Maven(directory.to_path_buf()));
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

pub(super) fn jps_message(root: &Path) -> String {
    format!(
        "krusty-toolchain build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
         {} is a JetBrains .iml project. .iml support remains a project-model extension and is not compiled by this command yet.",
        root.display()
    )
}

fn has_marker(directory: &Path, markers: &[&str]) -> bool {
    markers
        .iter()
        .any(|marker| directory.join(marker).is_file())
}
