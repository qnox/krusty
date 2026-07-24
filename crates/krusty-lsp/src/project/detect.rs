//! Which build system, if any, owns the worktree.

use std::path::{Path, PathBuf};

use super::bsp::{self, BspProvider};
use super::gradle::GradleProvider;
use super::maven::MavenProvider;
use super::model::ProviderKind;
use super::provider::{ExplicitProvider, NoBuildSystemProvider, ProjectProvider};

/// Ancestors searched above the opened directory. Editors are routinely pointed at a subdirectory
/// of a build; the bound keeps a stray marker in `$HOME` from claiming the project.
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

/// The build root and system found by walking up from `start`.
///
/// Gradle wins when both markers sit in the same directory: a `pom.xml` left beside a Gradle build
/// is nearly always a migration leftover, while the reverse is rare.
pub fn find_build_root(start: &Path) -> Option<(PathBuf, ProviderKind)> {
    for directory in start.ancestors().take(MAX_ANCESTORS) {
        if GRADLE_MARKERS
            .iter()
            .any(|marker| directory.join(marker).is_file())
        {
            return Some((directory.to_path_buf(), ProviderKind::Gradle));
        }
        if MAVEN_MARKERS
            .iter()
            .any(|marker| directory.join(marker).is_file())
        {
            return Some((directory.to_path_buf(), ProviderKind::Maven));
        }
    }
    None
}

/// Choose the producer for `root`.
///
/// Precedence: an explicit `-cp` short-circuits everything (the user stated the classpath); then a
/// BSP connection file, which the user set up deliberately and which is the richest, most current
/// source; then the Gradle and Maven probes; then nothing.
pub fn detect(root: &Path, explicit_classpath: &[PathBuf]) -> Box<dyn ProjectProvider> {
    if !explicit_classpath.is_empty() {
        return Box::new(ExplicitProvider::new(root, explicit_classpath.to_vec()));
    }
    if let Some(connection) = bsp::discover(root) {
        return Box::new(BspProvider::new(root, connection));
    }
    match find_build_root(root) {
        Some((build_root, ProviderKind::Gradle)) => Box::new(GradleProvider::new(build_root)),
        Some((build_root, ProviderKind::Maven)) => Box::new(MavenProvider::new(build_root)),
        _ => Box::new(NoBuildSystemProvider::new(root)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::testing::TempTree;

    #[test]
    fn gradle_and_maven_are_recognised_by_their_markers() {
        let gradle = TempTree::new("detect-gradle");
        gradle.write("settings.gradle.kts", "");
        assert_eq!(detect(gradle.root(), &[]).kind(), ProviderKind::Gradle);

        let maven = TempTree::new("detect-maven");
        maven.write("pom.xml", "<project/>");
        assert_eq!(detect(maven.root(), &[]).kind(), ProviderKind::Maven);
    }

    #[test]
    fn a_wrapper_alone_is_enough_to_recognise_gradle() {
        let tree = TempTree::new("detect-wrapper");
        tree.write("gradlew", "#!/bin/sh\n");
        assert_eq!(detect(tree.root(), &[]).kind(), ProviderKind::Gradle);
    }

    #[test]
    fn gradle_outranks_a_leftover_pom_in_the_same_directory() {
        let tree = TempTree::new("detect-both");
        tree.write("pom.xml", "<project/>");
        tree.write("build.gradle.kts", "");
        assert_eq!(detect(tree.root(), &[]).kind(), ProviderKind::Gradle);
    }

    #[test]
    fn the_build_root_is_found_from_a_subdirectory() {
        let tree = TempTree::new("detect-ancestor");
        tree.write("settings.gradle.kts", "");
        let nested = tree.directory("app/src/main/kotlin");
        assert_eq!(
            find_build_root(&nested),
            Some((tree.root.clone(), ProviderKind::Gradle))
        );
    }

    #[test]
    fn a_bsp_connection_file_outranks_the_gradle_and_maven_markers() {
        let tree = TempTree::new("detect-bsp");
        tree.write("settings.gradle.kts", "");
        tree.write("pom.xml", "<project/>");
        tree.write(
            ".bsp/sbt.json",
            r#"{"name":"sbt","argv":["sbt","bsp"],"languages":["kotlin"]}"#,
        );
        assert_eq!(detect(tree.root(), &[]).kind(), ProviderKind::Bsp);
    }

    #[test]
    fn an_explicit_classpath_outranks_a_bsp_connection_file() {
        let tree = TempTree::new("detect-explicit-bsp");
        tree.write(
            ".bsp/sbt.json",
            r#"{"name":"sbt","argv":["sbt"],"languages":["kotlin"]}"#,
        );
        assert_eq!(
            detect(tree.root(), &[PathBuf::from("/m2/a.jar")]).kind(),
            ProviderKind::Explicit
        );
    }

    #[test]
    fn an_explicit_classpath_outranks_every_marker() {
        let tree = TempTree::new("detect-explicit");
        tree.write("settings.gradle.kts", "");
        let provider = detect(tree.root(), &[PathBuf::from("/m2/a.jar")]);
        assert_eq!(provider.kind(), ProviderKind::Explicit);
        assert!(provider.watch_paths().is_empty());
    }

    #[test]
    fn a_directory_without_markers_falls_back_to_no_build_system() {
        let tree = TempTree::new("detect-none");
        tree.write("src/main/kotlin/App.kt", "fun main() {}");
        assert_eq!(detect(tree.root(), &[]).kind(), ProviderKind::None);
    }
}
