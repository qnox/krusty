//! Resolve Maven repository jars a project already named.
//!
//! A JPS library table records a Maven coordinate as a path under `$MAVEN_REPOSITORY$`. That is
//! the Maven case: one `mvn`/`mvnw` invocation resolves every missing coordinate with the user's
//! settings, mirrors, repositories, and credentials. A Gradle project does not come through here —
//! its classpath is the one Gradle already resolved. Editor refresh does not call this either.
//! A jar Maven cannot install at the path the library named stays missing, and the caller must not
//! treat that classpath entry as present.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::model::ProviderKind;
use super::runner::{Command, CommandRunner, ProcessRunner, MAVEN, SCAN_PROBE_TIMEOUT};

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

struct ArtifactCoordinate<'a> {
    group: &'a str,
    artifact: &'a str,
    version: &'a str,
    classifier: Option<&'a str>,
}

fn artifact_coordinate(coordinate: &str) -> Option<ArtifactCoordinate<'_>> {
    let mut parts = coordinate.split(':');
    let group = parts.next().filter(|part| !part.is_empty())?;
    let artifact = parts.next().filter(|part| !part.is_empty())?;
    let version = parts.next().filter(|part| !part.is_empty())?;
    let classifier = parts.next().filter(|part| !part.is_empty());
    if parts.next().is_some() {
        return None;
    }
    Some(ArtifactCoordinate {
        group,
        artifact,
        version,
        classifier,
    })
}

/// One Maven invocation for every missing coordinate. No repository URL is passed: Maven reads
/// settings, mirrors, and credentials itself, including `MAVEN_OPTS`.
///
/// A single POM stops at the first coordinate it cannot resolve, so each coordinate is its own
/// reactor module and the build uses `--fail-at-end`. Jars that landed in the local repository
/// stay installed; the rest stay missing.
pub fn maven_batch_command(root: &Path, reactor_pom: &Path) -> Command {
    Command::new(MAVEN.resolve(root), root)
        .args([
            "-B".to_string(),
            "--quiet".to_string(),
            "-fae".to_string(),
            "-f".to_string(),
            reactor_pom.to_string_lossy().into_owned(),
            "dependency:resolve".to_string(),
            "-DexcludeTransitive=true".to_string(),
        ])
        .with_timeout(SCAN_PROBE_TIMEOUT)
}

fn write_resolution_reactor(directory: &Path, jars: &[RepositoryJar]) -> io::Result<usize> {
    std::fs::create_dir_all(directory)?;
    let mut modules = String::new();
    let mut written = 0usize;
    for jar in jars {
        let Some(artifact) = artifact_coordinate(&jar.coordinate) else {
            continue;
        };
        let name = format!("m{written}");
        let module_dir = directory.join(&name);
        std::fs::create_dir_all(&module_dir)?;
        std::fs::write(module_dir.join("pom.xml"), module_pom(written, &artifact))?;
        modules.push_str(&format!("    <module>{name}</module>\n"));
        written += 1;
    }
    std::fs::write(directory.join("pom.xml"), aggregator_pom(&modules))?;
    Ok(written)
}

fn module_pom(index: usize, artifact: &ArtifactCoordinate<'_>) -> String {
    let classifier = artifact
        .classifier
        .map(|classifier| format!("      <classifier>{classifier}</classifier>\n"))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>krusty.resolution</groupId>
  <artifactId>jar-{index}</artifactId>
  <version>1</version>
  <dependencies>
    <dependency>
      <groupId>{group}</groupId>
      <artifactId>{name}</artifactId>
      <version>{version}</version>
      <type>jar</type>
{classifier}    </dependency>
  </dependencies>
</project>
"#,
        group = artifact.group,
        name = artifact.artifact,
        version = artifact.version,
    )
}

fn aggregator_pom(modules: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>krusty.resolution</groupId>
  <artifactId>repository-jars</artifactId>
  <version>1</version>
  <packaging>pom</packaging>
  <modules>
{modules}  </modules>
</project>
"#
    )
}

/// JPS libraries and Maven projects resolve missing repository jars with one Maven invocation.
/// Gradle (and every other producer) keeps the classpath its own probe returned. Editor refresh
/// does not call this.
pub fn ensure_repository_jars(
    kind: ProviderKind,
    root: &Path,
    classpath: &[PathBuf],
) -> InstallResult {
    let Some(repository) = maven_repository_dir() else {
        return InstallResult::default();
    };
    resolve_missing_jars(kind, classpath, &repository, |jars| {
        maven_resolve_all(root, jars)
    })
}

fn resolves_with_maven(kind: ProviderKind) -> bool {
    matches!(kind, ProviderKind::Jps | ProviderKind::Maven)
}

/// `resolve` runs once for the whole missing set. A failure still counts every jar that is now
/// present: `--fail-at-end` installs the coordinates Maven could resolve and leaves the rest.
fn resolve_missing_jars(
    kind: ProviderKind,
    classpath: &[PathBuf],
    repository: &Path,
    resolve: impl FnOnce(&[RepositoryJar]) -> io::Result<()>,
) -> InstallResult {
    if !resolves_with_maven(kind) {
        return InstallResult::default();
    }
    let jars = missing_repository_jars(classpath, repository);
    if jars.is_empty() {
        return InstallResult::default();
    }
    let _ = resolve(&jars);
    let mut result = InstallResult::default();
    for jar in &jars {
        if jar.path.is_file() {
            result.installed += 1;
        } else {
            result.failed += 1;
        }
    }
    result
}

fn maven_resolve_all(root: &Path, jars: &[RepositoryJar]) -> io::Result<()> {
    let directory = std::env::temp_dir().join(format!(
        "krusty-maven-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    let _cleanup = RemoveDir(directory.clone());
    let written = write_resolution_reactor(&directory, jars)?;
    if written == 0 {
        return Ok(());
    }
    let command = maven_batch_command(root, &directory.join("pom.xml"));
    let program = command.program.display().to_string();
    ProcessRunner
        .run(&command)
        .map(|_| ())
        .map_err(|error| io::Error::other(format!("{program}: {error}")))
}

struct RemoveDir(PathBuf);

impl Drop for RemoveDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    fn one_maven_invocation_lists_every_coordinate_and_passes_no_repository_url() {
        let tree = crate::project::testing::TempTree::new("maven-batch-command");
        tree.write("mvnw", "#!/bin/sh\n");
        let repository = tree.directory("m2");
        let sources = repository.join(
            "org/jetbrains/kotlin/kotlin-stdlib/2.5.0-Beta2/kotlin-stdlib-2.5.0-Beta2-sources.jar",
        );
        let jars = vec![
            repository_jar(&repository, &stdlib(&repository)).unwrap(),
            repository_jar(&repository, &sources).unwrap(),
        ];
        let reactor = tree.directory("reactor");
        assert_eq!(write_resolution_reactor(&reactor, &jars).unwrap(), 2);
        let aggregator = std::fs::read_to_string(reactor.join("pom.xml")).unwrap();
        assert!(aggregator.contains("<module>m0</module>"), "{aggregator}");
        assert!(aggregator.contains("<module>m1</module>"), "{aggregator}");
        assert!(
            aggregator.contains("<packaging>pom</packaging>"),
            "{aggregator}"
        );
        let first = std::fs::read_to_string(reactor.join("m0/pom.xml")).unwrap();
        assert!(
            first.contains("<groupId>org.jetbrains.kotlin</groupId>"),
            "{first}"
        );
        assert!(
            first.contains("<artifactId>kotlin-stdlib</artifactId>"),
            "{first}"
        );
        assert!(first.contains("<version>2.5.0-Beta2</version>"), "{first}");
        assert!(!first.contains("<classifier>"), "{first}");
        let second = std::fs::read_to_string(reactor.join("m1/pom.xml")).unwrap();
        assert!(
            second.contains("<classifier>sources</classifier>"),
            "{second}"
        );
        assert!(
            !aggregator.contains("http") && !first.contains("http") && !second.contains("curl"),
            "a resolution reactor must not name a repository"
        );

        let command = maven_batch_command(tree.root(), &reactor.join("pom.xml"));
        assert_eq!(command.program, tree.path("mvnw"));
        assert_eq!(command.working_directory, tree.root());
        assert_eq!(
            command.args,
            vec![
                "-B".to_string(),
                "--quiet".to_string(),
                "-fae".to_string(),
                "-f".to_string(),
                reactor.join("pom.xml").to_string_lossy().into_owned(),
                "dependency:resolve".to_string(),
                "-DexcludeTransitive=true".to_string(),
            ]
        );
        assert!(command.args.iter().all(|arg| {
            !arg.contains("http") && !arg.contains("curl") && !arg.contains("dependency:get")
        }));
        assert_eq!(command.timeout, SCAN_PROBE_TIMEOUT);
    }

    #[test]
    fn one_resolution_covers_every_missing_jar_and_a_partial_install_counts() {
        let tree = crate::project::testing::TempTree::new("maven-repository");
        let repository = tree.directory("m2");
        let present = stdlib(&repository);
        let missing = repository.join("com/example/lib/1.0/lib-1.0.jar");
        let classpath = vec![
            present.clone(),
            missing.clone(),
            tree.path("out/production/core"),
        ];

        let mut calls = 0;
        let failed = resolve_missing_jars(ProviderKind::Jps, &classpath, &repository, |jars| {
            calls += 1;
            assert_eq!(jars.len(), 2);
            Err(io::Error::other("not resolved"))
        });
        assert_eq!(calls, 1);
        assert_eq!(
            failed,
            InstallResult {
                installed: 0,
                failed: 2
            }
        );
        assert!(!present.exists());
        assert!(!missing.exists());

        let mut calls = 0;
        let installed =
            resolve_missing_jars(ProviderKind::Maven, &classpath, &repository, |jars| {
                calls += 1;
                assert_eq!(jars.len(), 2);
                for jar in jars {
                    assert!(!maven_artifact_parameter(&jar.coordinate).contains("http"));
                    if let Some(parent) = jar.path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&jar.path, &jar.coordinate)?;
                }
                Err(io::Error::other("a later module failed"))
            });
        assert_eq!(calls, 1);
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

        let again = resolve_missing_jars(ProviderKind::Jps, &classpath, &repository, |_jars| {
            panic!("an installed jar must not be resolved again");
        });
        assert_eq!(again, InstallResult::default());
    }

    #[test]
    fn a_resolve_that_does_not_install_the_named_path_is_not_counted() {
        let tree = crate::project::testing::TempTree::new("maven-repository-absent");
        let repository = tree.directory("m2");
        let missing = repository.join("com/example/lib/1.0/lib-1.0.jar");
        let partial = stdlib(&repository);
        let classpath = vec![missing.clone(), partial.clone()];
        let result = resolve_missing_jars(ProviderKind::Jps, &classpath, &repository, |jars| {
            assert_eq!(jars.len(), 2);
            let installed = jars
                .iter()
                .find(|jar| jar.path == partial)
                .expect("the stdlib coordinate");
            if let Some(parent) = installed.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&installed.path, "jar")?;
            Ok(())
        });
        assert_eq!(
            result,
            InstallResult {
                installed: 1,
                failed: 1
            }
        );
        assert!(!missing.exists());
        assert!(partial.is_file());
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
        let resolved = resolve_missing_jars(ProviderKind::Jps, &classpath, &repository, |jars| {
            assert_eq!(jars.len(), 1);
            assert_eq!(
                maven_artifact_parameter(&jars[0].coordinate),
                "org.jetbrains.kotlin:kotlin-stdlib:2.5.0-Beta2:jar"
            );
            if let Some(parent) = jars[0].path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&jars[0].path, "jar")?;
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
