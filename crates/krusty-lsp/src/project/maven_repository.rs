//! Resolve Maven repository jars a project already named.
//!
//! A JPS library table records a Maven coordinate as a path under `$MAVEN_REPOSITORY$`. That is
//! the Maven case: `mvn`/`mvnw` resolves the coordinate with the user's settings, mirrors,
//! repositories, and credentials. A Gradle project does not come through here — its classpath is
//! the one Gradle already resolved. A jar Maven cannot install at the path the library named stays
//! missing, and the caller must not treat that classpath entry as present.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::model::ProviderKind;
use super::runner::{Command, CommandRunner, ProcessRunner, MAVEN};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryJar {
    pub path: PathBuf,
    pub coordinate: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallResult {
    pub installed: usize,
    pub failed: usize,
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
    Some(RepositoryJar {
        path: jar.to_path_buf(),
        coordinate,
    })
}

/// `dependency:get` artifact parameter. Packaging stays `jar`; a classifier is the fifth field,
/// because the fourth field is packaging and a bare `g:a:v:sources` would ask for packaging
/// `sources`.
pub fn maven_artifact_parameter(coordinate: &str) -> String {
    let mut parts = coordinate.split(':');
    let group = parts.next().unwrap_or("");
    let artifact = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    match parts.next() {
        Some(classifier) if !classifier.is_empty() => {
            format!("{group}:{artifact}:{version}:jar:{classifier}")
        }
        _ => format!("{group}:{artifact}:{version}:jar"),
    }
}

/// The project's `mvnw` when it has one, otherwise `mvn` on `PATH`. No repository URL is passed:
/// Maven reads settings, mirrors, and credentials itself, including `MAVEN_OPTS`.
pub fn maven_get_command(root: &Path, artifact: &str) -> Command {
    Command::new(MAVEN.resolve(root), root).args([
        "-B".to_string(),
        "--quiet".to_string(),
        "dependency:get".to_string(),
        format!("-Dartifact={artifact}"),
        "-Dtransitive=false".to_string(),
    ])
}

/// Fetch each missing repository jar. `resolve` installs the artifact at the path the library
/// named. A failure, or a success that leaves that path absent, keeps the jar missing.
pub fn install_missing_repository_jars(
    classpath: &[PathBuf],
    repository: &Path,
    mut resolve: impl FnMut(&RepositoryJar) -> io::Result<()>,
) -> InstallResult {
    let mut result = InstallResult::default();
    for jar in missing_repository_jars(classpath, repository) {
        match resolve(&jar) {
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

/// JPS libraries and Maven projects resolve missing repository jars with Maven. Gradle (and every
/// other producer) keeps the classpath its own probe returned.
pub fn ensure_repository_jars(
    kind: ProviderKind,
    root: &Path,
    classpath: &[PathBuf],
) -> InstallResult {
    let Some(repository) = maven_repository_dir() else {
        return InstallResult::default();
    };
    resolve_missing_jars(kind, classpath, &repository, |jar| {
        maven_resolve(root, &jar.coordinate, &jar.path)
    })
}

fn resolves_with_maven(kind: ProviderKind) -> bool {
    matches!(kind, ProviderKind::Jps | ProviderKind::Maven)
}

fn resolve_missing_jars(
    kind: ProviderKind,
    classpath: &[PathBuf],
    repository: &Path,
    resolve: impl FnMut(&RepositoryJar) -> io::Result<()>,
) -> InstallResult {
    if !resolves_with_maven(kind) {
        return InstallResult::default();
    }
    install_missing_repository_jars(classpath, repository, resolve)
}

fn maven_resolve(root: &Path, coordinate: &str, destination: &Path) -> io::Result<()> {
    let command = maven_get_command(root, &maven_artifact_parameter(coordinate));
    let program = command.program.display().to_string();
    let output = ProcessRunner
        .run(&command)
        .map_err(|error| io::Error::other(format!("{program}: {error}")))?;
    if !output.succeeded() {
        return Err(io::Error::other(format!(
            "{program} dependency:get failed for {coordinate}"
        )));
    }
    if !destination.is_file() {
        return Err(io::Error::other(format!(
            "{program} resolved {coordinate} but {} is still absent",
            destination.display()
        )));
    }
    Ok(())
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
    fn a_repository_jar_names_its_maven_coordinate() {
        let repository = Path::new("/m2");
        let jar = repository_jar(repository, &stdlib(repository)).unwrap();
        assert_eq!(
            jar.coordinate,
            "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2"
        );
        assert_eq!(
            maven_artifact_parameter(&jar.coordinate),
            "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2:jar"
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
        assert_eq!(
            maven_artifact_parameter(&jar.coordinate),
            "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2:jar:sources"
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
    fn maven_get_uses_the_project_maven_and_passes_no_repository_url() {
        let tree = crate::project::testing::TempTree::new("maven-get-command");
        tree.write("mvnw", "#!/bin/sh\n");
        let artifact = "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2:jar";
        let command = maven_get_command(tree.root(), artifact);
        assert_eq!(command.program, tree.path("mvnw"));
        assert_eq!(command.working_directory, tree.root());
        assert_eq!(
            command.args,
            vec![
                "-B".to_string(),
                "--quiet".to_string(),
                "dependency:get".to_string(),
                format!("-Dartifact={artifact}"),
                "-Dtransitive=false".to_string(),
            ]
        );
        assert!(command
            .args
            .iter()
            .all(|arg| !arg.contains("http") && !arg.contains("curl")));
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

        let failed = install_missing_repository_jars(&classpath, &repository, |_jar| {
            Err(io::Error::other("not resolved"))
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

        let installed = install_missing_repository_jars(&classpath, &repository, |jar| {
            assert!(!maven_artifact_parameter(&jar.coordinate).contains("http"));
            if let Some(parent) = jar.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&jar.path, &jar.coordinate)?;
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

        let again = install_missing_repository_jars(&classpath, &repository, |_jar| {
            panic!("an installed jar must not be resolved again");
        });
        assert_eq!(again, InstallResult::default());
    }

    #[test]
    fn a_resolve_that_does_not_install_the_named_path_is_not_counted() {
        let tree = crate::project::testing::TempTree::new("maven-repository-absent");
        let repository = tree.directory("m2");
        let missing = repository.join("com/example/lib/1.0/lib-1.0.jar");
        let result =
            install_missing_repository_jars(&[missing.clone()], &repository, |_jar| Ok(()));
        assert_eq!(
            result,
            InstallResult {
                installed: 0,
                failed: 1
            }
        );
        assert!(!missing.exists());
    }

    #[test]
    fn gradle_keeps_its_own_classpath_and_does_not_resolve_with_maven() {
        let tree = crate::project::testing::TempTree::new("gradle-classpath");
        let repository = tree.directory("m2");
        let missing = stdlib(&repository);
        let classpath = vec![missing.clone()];
        for kind in [
            ProviderKind::Gradle,
            ProviderKind::Bsp,
            ProviderKind::Explicit,
            ProviderKind::None,
        ] {
            let result = resolve_missing_jars(kind, &classpath, &repository, |_| {
                panic!(
                    "{} must not resolve repository jars with maven",
                    kind.as_str()
                );
            });
            assert_eq!(result, InstallResult::default(), "{}", kind.as_str());
        }
        let resolved = resolve_missing_jars(ProviderKind::Jps, &classpath, &repository, |jar| {
            assert_eq!(
                maven_artifact_parameter(&jar.coordinate),
                "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2:jar"
            );
            if let Some(parent) = jar.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&jar.path, "jar")?;
            Ok(())
        });
        assert_eq!(
            resolved,
            InstallResult {
                installed: 1,
                failed: 0
            }
        );
        assert!(missing.is_file());
    }
}
