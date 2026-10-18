//! Where a pinned jar may already be on disk: krusty's cache, and the caches of the Gradle and Maven
//! builds the machine has run. A Kotlin Gradle build resolves `kotlin-compiler-embeddable` at its
//! Kotlin version, so the jars are usually there before krusty ever needs them.

use std::path::PathBuf;

use crate::environment::Environment;
use crate::pins::ArtifactPin;

pub(crate) fn cached(env: &Environment, pin: &ArtifactPin) -> PathBuf {
    env.cache_dir.join("maven").join(pin.repository_path())
}

/// Copies other builds may hold, each with the name of the cache it is in. Their content is
/// unverified until checked against the pin.
pub(crate) fn installed_copies(
    env: &Environment,
    pin: &ArtifactPin,
) -> Vec<(PathBuf, &'static str)> {
    let mut copies = Vec::new();
    if let Some(gradle) = &env.gradle_user_home {
        // `caches/modules-2/files-2.1/<group>/<artifact>/<version>/<sha1 of the file>/<file>`.
        let dir = gradle
            .join("caches/modules-2/files-2.1")
            .join(&pin.group)
            .join(&pin.artifact)
            .join(&pin.version);
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join(pin.file_name()))
            .filter(|file| file.is_file())
            .collect();
        found.sort();
        copies.extend(found.into_iter().map(|file| (file, "the Gradle cache")));
    }
    if let Some(maven) = &env.maven_local {
        let file = maven.join(pin.repository_path());
        if file.is_file() {
            copies.push((file, "the local Maven repository"));
        }
    }
    copies
}

pub(crate) fn url(env: &Environment, pin: &ArtifactPin) -> String {
    format!("{}/{}", env.maven_repository, pin.repository_path())
}
