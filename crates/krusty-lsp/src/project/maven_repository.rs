//! Download Maven repository jars a parity scan already named.
//!
//! A JPS library table records the resolved coordinate as a path under `$MAVEN_REPOSITORY$`. The
//! parity command fetches that artifact from Maven Central into the path the library table names.
//! The editor refresh does not: it runs on the analysis thread. A jar that is not in that layout,
//! or whose download fails, stays missing. Callers must not treat the classpath as present until
//! the file exists.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const MAVEN_CENTRAL: &str = "https://repo.maven.apache.org/maven2";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryJar {
    pub path: PathBuf,
    pub coordinate: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallResult {
    pub installed: usize,
    pub failed: usize,
}

/// Where a missing Maven-repository jar may be downloaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryFetch {
    /// Editor refresh. Missing jars stay missing; nothing is fetched.
    Editor,
    /// `krusty-lsp parity`.
    Parity,
}

impl RepositoryFetch {
    /// Install missing repository jars when this is the parity scan.
    ///
    /// The editor value returns without reading the local repository or calling out to the network.
    pub fn fetch_named_jars(self, classpath: &[PathBuf]) -> InstallResult {
        let repository = match self {
            Self::Editor => None,
            Self::Parity => maven_repository_dir(),
        };
        fetch_repository_jars(self, classpath, repository.as_deref(), curl_fetch)
    }
}

/// Fetch missing repository jars when `site` is [`RepositoryFetch::Parity`].
///
/// `Editor` returns before `fetch` is called, even when `repository` names a missing jar.
fn fetch_repository_jars(
    site: RepositoryFetch,
    classpath: &[PathBuf],
    repository: Option<&Path>,
    fetch: impl FnMut(&str, &Path) -> io::Result<()>,
) -> InstallResult {
    if site != RepositoryFetch::Parity {
        return InstallResult::default();
    }
    let Some(repository) = repository else {
        return InstallResult::default();
    };
    install_missing_repository_jars(classpath, repository, fetch)
}

pub fn maven_repository_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".m2").join("repository"))
}

/// Classpath entries that are missing jars in `repository`'s Maven layout.
pub fn missing_repository_jars(classpath: &[PathBuf], repository: &Path) -> Vec<RepositoryJar> {
    classpath
        .iter()
        .filter(|path| !path.exists())
        .filter_map(|path| repository_jar(repository, path))
        .collect()
}

pub fn repository_jar(repository: &Path, jar: &Path) -> Option<RepositoryJar> {
    let relative = jar.strip_prefix(repository).ok()?;
    let mut segments = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment.to_str()?;
                if !artifact_token(segment) {
                    return None;
                }
                segments.push(segment);
            }
            _ => return None,
        }
    }
    if segments.len() < 4 {
        return None;
    }
    let file = segments.pop()?;
    let version = segments.pop()?;
    let artifact = segments.pop()?;
    if segments.is_empty() {
        return None;
    }
    let stem = file.strip_suffix(".jar")?;
    let prefix = format!("{artifact}-{version}");
    let coordinate = if stem == prefix {
        format!("{}:{artifact}:{version}", segments.join("."))
    } else {
        let classifier = stem.strip_prefix(&format!("{prefix}-"))?;
        if !artifact_token(classifier) {
            return None;
        }
        format!("{}:{artifact}:{version}:{classifier}", segments.join("."))
    };
    segments.push(artifact);
    segments.push(version);
    segments.push(file);
    Some(RepositoryJar {
        path: jar.to_path_buf(),
        coordinate,
        url: format!("{MAVEN_CENTRAL}/{}", segments.join("/")),
    })
}

/// Fetch each missing repository jar. `fetch` writes the artifact bytes to the destination.
/// A failed fetch leaves that path absent.
pub fn install_missing_repository_jars(
    classpath: &[PathBuf],
    repository: &Path,
    mut fetch: impl FnMut(&str, &Path) -> io::Result<()>,
) -> InstallResult {
    let mut result = InstallResult::default();
    for jar in missing_repository_jars(classpath, repository) {
        match fetch(&jar.url, &jar.path) {
            Ok(()) if jar.path.is_file() => result.installed += 1,
            _ => {
                if !jar.path.is_file() {
                    result.failed += 1;
                }
            }
        }
    }
    result
}

/// Download one Maven Central artifact with `curl` into `destination`.
pub fn curl_fetch(url: &str, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = destination.with_extension("partial");
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "60", "-o"])
        .arg(&temporary)
        .arg(url)
        .output()
        .map_err(|error| io::Error::other(format!("curl: {error}")))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&temporary);
        return Err(io::Error::other(format!(
            "curl exited {} for {url}",
            output.status
        )));
    }
    std::fs::rename(&temporary, destination).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

pub fn ensure_maven_repository_jars(classpath: &[PathBuf]) -> InstallResult {
    RepositoryFetch::Parity.fetch_named_jars(classpath)
}

fn artifact_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdlib(repository: &Path) -> PathBuf {
        repository
            .join("org/jetbrains/kotlin/kotlin-stdlib/2.5.0-Beta2/kotlin-stdlib-2.5.0-Beta2.jar")
    }

    #[test]
    fn a_repository_jar_names_its_maven_central_artifact() {
        let repository = Path::new("/m2");
        let jar = repository_jar(repository, &stdlib(repository)).unwrap();
        assert_eq!(
            jar.coordinate,
            "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2"
        );
        assert_eq!(
            jar.url,
            "https://repo.maven.apache.org/maven2/org/jetbrains/kotlin/kotlin-stdlib/2.5.0-Beta2/kotlin-stdlib-2.5.0-Beta2.jar"
        );
    }

    #[test]
    fn a_classifier_jar_keeps_the_classifier() {
        let repository = Path::new("/m2");
        let jar = repository.join(
            "org/jetbrains/kotlin/kotlin-stdlib/2.5.0-Beta2/kotlin-stdlib-2.5.0-Beta2-sources.jar",
        );
        let jar = repository_jar(repository, &jar).unwrap();
        assert_eq!(
            jar.coordinate,
            "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2:sources"
        );
    }

    #[test]
    fn a_path_outside_the_repository_or_with_a_bad_name_is_not_fetched() {
        let repository = Path::new("/m2");
        assert!(repository_jar(repository, Path::new("/repo/out/production/core")).is_none());
        assert!(repository_jar(
            repository,
            &repository.join("org/example/lib/1.0/not-the-artifact.jar")
        )
        .is_none());
        assert!(
            repository_jar(repository, &repository.join("org/../lib/1.0/lib-1.0.jar")).is_none()
        );
    }

    #[test]
    fn a_failed_fetch_leaves_the_jar_missing_and_a_successful_one_installs_it() {
        let tree = crate::project::testing::TempTree::new("maven-repository");
        let repository = tree.directory("m2");
        let present = stdlib(&repository);
        let missing = repository.join("com/example/lib/1.0/lib-1.0.jar");
        let classpath = vec![
            present.clone(),
            missing.clone(),
            tree.path("out/production/core"),
        ];

        let failed = install_missing_repository_jars(&classpath, &repository, |_url, _path| {
            Err(io::Error::other("offline"))
        });
        assert_eq!(
            failed,
            InstallResult {
                installed: 0,
                failed: 2
            }
        );
        assert!(!present.exists());
        assert!(!missing.exists());

        let installed = install_missing_repository_jars(&classpath, &repository, |url, path| {
            assert!(url.starts_with("https://repo.maven.apache.org/maven2/"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, url)?;
            Ok(())
        });
        assert_eq!(
            installed,
            InstallResult {
                installed: 2,
                failed: 0
            }
        );
        assert!(present.is_file());
        assert!(missing.is_file());
        assert!(!tree.path("out/production/core").exists());

        let again = install_missing_repository_jars(&classpath, &repository, |_url, _path| {
            panic!("an installed jar must not be fetched again");
        });
        assert_eq!(again, InstallResult::default());
    }

    #[test]
    fn the_editor_fetch_gate_does_not_download_a_missing_jar() {
        let tree = crate::project::testing::TempTree::new("editor-fetch-gate");
        let repository = tree.directory("m2");
        let missing = repository.join("com/example/lib/1.0/lib-1.0.jar");
        let mut calls = 0;
        let result = fetch_repository_jars(
            RepositoryFetch::Editor,
            std::slice::from_ref(&missing),
            Some(&repository),
            |_url, _path| {
                calls += 1;
                Ok(())
            },
        );
        assert_eq!(calls, 0);
        assert_eq!(result, InstallResult::default());
        assert!(!missing.exists());
        assert_eq!(
            RepositoryFetch::Editor.fetch_named_jars(std::slice::from_ref(&missing)),
            InstallResult::default()
        );
    }

    #[test]
    fn the_parity_fetch_gate_downloads_a_missing_repository_jar() {
        let tree = crate::project::testing::TempTree::new("parity-fetch-gate");
        let repository = tree.directory("m2");
        let missing = repository.join("com/example/lib/1.0/lib-1.0.jar");
        let mut calls = 0;
        let result = fetch_repository_jars(
            RepositoryFetch::Parity,
            std::slice::from_ref(&missing),
            Some(&repository),
            |url, path| {
                calls += 1;
                assert!(url.starts_with("https://repo.maven.apache.org/maven2/"));
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, url)?;
                Ok(())
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(
            result,
            InstallResult {
                installed: 1,
                failed: 0
            }
        );
        assert!(missing.is_file());
    }
}
