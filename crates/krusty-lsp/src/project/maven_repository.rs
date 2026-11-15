//! Resolve named JPS repository libraries with Maven's resolver.
//!
//! A repository library maps one `maven-id` onto the jars in its `CLASSES` roots.
//! The root coordinate keeps its transitive closure unless the library sets
//! `include-transitive-deps="false"`. Each other `CLASSES` jar is a coordinate of
//! its own: it is the version IntelliJ recorded, and a fresh resolve of the root
//! can install a different version at a different path. Named `<exclude>` entries
//! still apply to the root. A pinned jar is requested on its own, so the file at
//! the recorded path is what gets installed.
//!
//! One `RepositorySystem` request resolves every library that still has a missing
//! jar: `resolveDependencies` for a transitive root and `resolveArtifact` for a
//! pinned jar. Settings, mirrors, and credentials come from the embedder session.
//! Editor refresh does not call this. A library whose jars do not all exist
//! afterwards stays missing.

use std::io;
use std::path::Path;

use super::model::{ProviderKind, RepositoryLibrary};
use super::tooling::{self, MavenArtifactRequest, MavenBatch, MavenExclusion};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallResult {
    pub installed: usize,
    pub failed: usize,
}

/// One resolver request for every library that still has a missing jar. No repository
/// URL is named here: the embedder session supplies mirrors and credentials. A library
/// that includes transitives asks `resolveDependencies` for them, and a pinned jar asks
/// `resolveArtifact` for the recorded version only.
fn resolution_batch(libraries: &[&RepositoryLibrary]) -> MavenBatch {
    let mut artifacts = Vec::new();
    for library in libraries {
        artifacts.push(MavenArtifactRequest {
            group: library.group.clone(),
            artifact: library.artifact.clone(),
            version: library.version.clone(),
            extension: library.packaging.clone(),
            classifier: String::new(),
            transitive: library.include_transitive,
            exclusions: library
                .excluded
                .iter()
                .map(|excluded| MavenExclusion {
                    group: excluded.group.clone(),
                    artifact: excluded.artifact.clone(),
                })
                .collect(),
        });
        for pin in pinned_jars(library) {
            artifacts.push(MavenArtifactRequest {
                group: pin.group,
                artifact: pin.artifact,
                version: pin.version,
                extension: pin.packaging,
                classifier: pin.classifier.unwrap_or_default(),
                transitive: false,
                exclusions: Vec::new(),
            });
        }
    }
    MavenBatch { artifacts }
}

/// A `CLASSES` jar under `~/.m2/repository` whose filename is the Maven layout
/// `{artifact}-{version}[-{classifier}].{ext}`.
struct PinnedJar {
    group: String,
    artifact: String,
    version: String,
    classifier: Option<String>,
    packaging: String,
}

fn pinned_jars(library: &RepositoryLibrary) -> Vec<PinnedJar> {
    let mut pins = Vec::new();
    for jar in &library.jars {
        let Some(pin) = pinned_jar(jar) else {
            continue;
        };
        let same_as_root = pin.classifier.is_none()
            && pin.group == library.group
            && pin.artifact == library.artifact
            && pin.version == library.version
            && pin.packaging == library.packaging;
        if same_as_root {
            continue;
        }
        if pins.iter().any(|existing: &PinnedJar| {
            existing.group == pin.group
                && existing.artifact == pin.artifact
                && existing.version == pin.version
                && existing.classifier == pin.classifier
                && existing.packaging == pin.packaging
        }) {
            continue;
        }
        pins.push(pin);
    }
    pins
}

fn pinned_jar(jar: &Path) -> Option<PinnedJar> {
    let text = jar.to_string_lossy();
    let marker = "/.m2/repository/";
    let relative = text.split_once(marker)?.1;
    let mut parts = relative.split('/').collect::<Vec<_>>();
    let filename = parts.pop()?;
    let version = parts.pop()?.to_string();
    let artifact = parts.pop()?.to_string();
    if parts.is_empty() || version.is_empty() || artifact.is_empty() {
        return None;
    }
    let group = parts.join(".");
    let prefix = format!("{artifact}-{version}");
    let rest = filename.strip_prefix(&prefix)?;
    let (classifier, packaging) = if let Some(classified) = rest.strip_prefix('-') {
        let (classifier, packaging) = classified.rsplit_once('.')?;
        if classifier.is_empty() || packaging.is_empty() {
            return None;
        }
        (Some(classifier.to_string()), packaging.to_string())
    } else {
        let packaging = rest.strip_prefix('.')?;
        if packaging.is_empty() || packaging.contains('.') {
            return None;
        }
        (None, packaging.to_string())
    };
    Some(PinnedJar {
        group,
        artifact,
        version,
        classifier,
        packaging,
    })
}

/// JPS only. Gradle and Maven projects keep the classpath their own probe returned.
/// Editor refresh does not call this.
pub fn ensure_repository_libraries(
    kind: ProviderKind,
    root: &Path,
    libraries: &[RepositoryLibrary],
) -> InstallResult {
    let _ = root;
    resolve_missing_libraries(kind, libraries, maven_resolve_all)
}

fn library_missing(library: &RepositoryLibrary) -> bool {
    library.jars.iter().any(|jar| !jar.is_file())
}

fn library_complete(library: &RepositoryLibrary) -> bool {
    !library.jars.is_empty() && library.jars.iter().all(|jar| jar.is_file())
}

/// `resolve` runs once. A library counts as installed only when every jar it lists
/// is present afterwards. A jar that landed without its siblings does not make the
/// library resolved.
fn resolve_missing_libraries(
    kind: ProviderKind,
    libraries: &[RepositoryLibrary],
    resolve: impl FnOnce(&[&RepositoryLibrary]) -> io::Result<()>,
) -> InstallResult {
    if kind != ProviderKind::Jps {
        return InstallResult::default();
    }
    let pending: Vec<&RepositoryLibrary> = libraries
        .iter()
        .filter(|library| library_missing(library))
        .collect();
    if pending.is_empty() {
        return InstallResult::default();
    }
    let _ = resolve(&pending);
    let mut result = InstallResult::default();
    for library in &pending {
        if library_complete(library) {
            result.installed += 1;
        } else {
            result.failed += 1;
        }
    }
    result
}

fn maven_resolve_all(libraries: &[&RepositoryLibrary]) -> io::Result<()> {
    let batch = resolution_batch(libraries);
    if batch.artifacts.is_empty() {
        return Ok(());
    }
    tooling::resolve_maven(&batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::project::model::ExcludedMavenArtifact;

    fn library(name: &str, coordinate: &str, jars: Vec<PathBuf>) -> RepositoryLibrary {
        let mut parts = coordinate.split(':');
        RepositoryLibrary {
            name: name.to_string(),
            group: parts.next().unwrap().to_string(),
            artifact: parts.next().unwrap().to_string(),
            version: parts.next().unwrap().to_string(),
            packaging: "jar".to_string(),
            include_transitive: true,
            excluded: Vec::new(),
            jars,
        }
    }

    fn exclusion(group: &str, artifact: &str) -> ExcludedMavenArtifact {
        ExcludedMavenArtifact {
            group: group.to_string(),
            artifact: artifact.to_string(),
        }
    }

    fn write_jars(library: &RepositoryLibrary) -> io::Result<()> {
        for jar in &library.jars {
            if let Some(parent) = jar.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(jar, "jar")?;
        }
        Ok(())
    }

    #[test]
    fn a_library_pins_every_recorded_jar_and_keeps_the_root_transitive() {
        let tree = crate::project::testing::TempTree::new("maven-library-reactor");
        let repository = tree.root().join(".m2/repository");
        let classic =
            repository.join("ch/qos/logback/logback-classic/1.3.14/logback-classic-1.3.14.jar");
        let core = repository.join("ch/qos/logback/logback-core/1.3.14/logback-core-1.3.14.jar");
        let mut logback = library(
            "fleet:logback-classic",
            "ch.qos.logback:logback-classic:1.3.14",
            vec![classic, core],
        );
        logback.excluded = vec![
            exclusion("org.slf4j", "slf4j-api"),
            exclusion("com.sun.mail", "javax.mail"),
        ];
        let dex = repository
            .join("com/android/tools/smali/smali-dexlib2/3.0.10/smali-dexlib2-3.0.10.jar");
        let mut dexlib = library(
            "google-dexlib2",
            "com.android.tools.smali:smali-dexlib2:3.0.10",
            vec![dex],
        );
        dexlib.include_transitive = false;

        let batch = resolution_batch(&[&logback, &dexlib]);
        assert_eq!(batch.artifacts.len(), 3, "{:?}", batch.artifacts);
        let root = &batch.artifacts[0];
        assert_eq!(root.artifact, "logback-classic");
        assert!(root.transitive);
        assert_eq!(
            root.exclusions
                .iter()
                .map(|exclusion| exclusion.artifact.as_str())
                .collect::<Vec<_>>(),
            vec!["slf4j-api", "javax.mail"]
        );
        assert!(root
            .exclusions
            .iter()
            .all(|exclusion| exclusion.group != "*"));
        let pinned = &batch.artifacts[1];
        assert_eq!(pinned.artifact, "logback-core");
        assert_eq!(pinned.version, "1.3.14");
        assert!(
            !pinned.transitive,
            "a pinned jar is resolveArtifact, not a tree"
        );
        assert!(pinned.exclusions.is_empty());
        let direct = &batch.artifacts[2];
        assert_eq!(direct.artifact, "smali-dexlib2");
        assert!(!direct.transitive);
        assert!(batch.artifacts.iter().all(|artifact| {
            !artifact.group.contains("http") && !artifact.version.contains("curl")
        }));

        crate::project::tooling::reset_test_hooks();
        crate::project::tooling::set_maven_response(Ok(()));
        maven_resolve_all(&[&logback, &dexlib]).unwrap();
        let batches = crate::project::tooling::maven_batches();
        assert_eq!(batches.len(), 1, "one RepositorySystem request");
        assert_eq!(batches[0], batch);
    }

    #[test]
    fn a_pinned_transitive_keeps_the_version_in_its_repository_path() {
        let tree = crate::project::testing::TempTree::new("maven-library-pin");
        let repository = tree.root().join(".m2/repository");
        let archive = repository.join(
            "com/jetbrains/intellij/idea/idea-community-build-zip/252.11244/idea-community-build-zip-252.11244.jar",
        );
        let netty = repository.join("io/netty/netty-buffer/4.2.0.RC2/netty-buffer-4.2.0.RC2.jar");
        let library = library(
            "fleet:jetbrains.intellij.idea.community.build.zip",
            "com.jetbrains.intellij.idea:idea-community-build-zip:252.11244",
            vec![archive, netty],
        );
        let batch = resolution_batch(&[&library]);
        let pinned = batch
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact == "netty-buffer")
            .unwrap();
        assert_eq!(pinned.version, "4.2.0.RC2");
        assert!(!pinned.transitive);
        assert!(batch
            .artifacts
            .iter()
            .all(|artifact| artifact.version != "4.2.18.Final"));
    }

    #[test]
    fn a_library_stays_missing_until_every_artifact_lands() {
        let tree = crate::project::testing::TempTree::new("maven-library-partial");
        let repository = tree.directory("m2");
        let classic =
            repository.join("ch/qos/logback/logback-classic/1.3.14/logback-classic-1.3.14.jar");
        let core = repository.join("ch/qos/logback/logback-core/1.3.14/logback-core-1.3.14.jar");
        let logback = library(
            "fleet:logback-classic",
            "ch.qos.logback:logback-classic:1.3.14",
            vec![classic.clone(), core.clone()],
        );
        let dex = repository
            .join("com/android/tools/smali/smali-dexlib2/3.0.10/smali-dexlib2-3.0.10.jar");
        let mut dexlib = library(
            "google-dexlib2",
            "com.android.tools.smali:smali-dexlib2:3.0.10",
            vec![dex.clone()],
        );
        dexlib.include_transitive = false;
        let libraries = vec![logback, dexlib];

        let mut calls = 0;
        let partial = resolve_missing_libraries(ProviderKind::Jps, &libraries, |pending| {
            calls += 1;
            assert_eq!(pending.len(), 2);
            assert!(pending[0].include_transitive);
            assert!(!pending[1].include_transitive);
            assert_eq!(pending[0].jars.len(), 2);
            if let Some(parent) = pending[0].jars[0].parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&pending[0].jars[0], "jar")?;
            Ok(())
        });
        assert_eq!(calls, 1);
        assert_eq!(
            partial,
            InstallResult {
                installed: 0,
                failed: 2
            }
        );
        assert!(classic.is_file());
        assert!(!core.exists());
        assert!(!dex.exists());

        let installed = resolve_missing_libraries(ProviderKind::Jps, &libraries, |pending| {
            assert_eq!(pending.len(), 2);
            for library in pending {
                write_jars(library)?;
            }
            Err(io::Error::other("a later module failed"))
        });
        assert_eq!(
            installed,
            InstallResult {
                installed: 2,
                failed: 0
            }
        );
        assert!(core.is_file());
        assert!(dex.is_file());

        let again = resolve_missing_libraries(ProviderKind::Jps, &libraries, |_| {
            panic!("an installed library must not be resolved again");
        });
        assert_eq!(again, InstallResult::default());
    }

    #[test]
    fn other_providers_do_not_resolve_jps_libraries() {
        let tree = crate::project::testing::TempTree::new("maven-library-other");
        let missing = tree.path("missing.jar");
        let libraries = vec![library(
            "kotlin-stdlib",
            "org.jetbrains.kotlin:kotlin-stdlib:2.2.20",
            vec![missing],
        )];
        for kind in [
            ProviderKind::Gradle,
            ProviderKind::Maven,
            ProviderKind::Bsp,
            ProviderKind::Explicit,
            ProviderKind::None,
        ] {
            let result = resolve_missing_libraries(kind, &libraries, |_| {
                panic!("{} must not resolve repository libraries", kind.as_str());
            });
            assert_eq!(result, InstallResult::default(), "{}", kind.as_str());
        }
    }
}
