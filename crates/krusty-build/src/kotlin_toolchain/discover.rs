//! Which project a `kotlin build` invocation is standing in.
//!
//! The Kotlin Toolchain layout (`module.yaml` / `project.yaml`) is the project `kotlin build`
//! compiles. A `project.yaml` owns every `module.yaml` beneath it, matching the toolchain rule that
//! a module file included by a parent project is not its own project. Gradle and a JetBrains `.iml`
//! model are recognized so they are not read as that layout; compiling them stays with their own
//! project-model extensions.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProjectKind {
    Toolchain(PathBuf),
    Gradle(PathBuf),
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
        if project.is_none()
            && module.is_none()
            && GRADLE_MARKERS
                .iter()
                .any(|marker| directory.join(marker).is_file())
        {
            return Some(ProjectKind::Gradle(directory.to_path_buf()));
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
        ProjectKind::Gradle(root) => Some(format!(
            "kotlin build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
             {} is a Gradle project. Gradle remains a project-model extension and is not compiled by this command yet.",
            root.display()
        )),
        ProjectKind::Jps(root) => Some(format!(
            "kotlin build compiles Kotlin Toolchain projects (module.yaml or project.yaml).\n\
             {} is a JetBrains .iml project. .iml support remains a project-model extension and is not compiled by this command yet.",
            root.display()
        )),
    }
}
