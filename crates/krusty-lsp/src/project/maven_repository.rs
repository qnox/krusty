//! Resolve named JPS repository libraries with Maven.
//!
//! A repository library maps one `maven-id` onto the jars in its `CLASSES` roots.
//! Those extra jars are the transitive closure of that coordinate, recorded after
//! IntelliJ resolved it. They are not coordinates of their own. Transitives are
//! therefore requested unless the library sets `include-transitive-deps="false"`
//! (every such library in the IntelliJ model lists only its own jar). Named
//! `<exclude>` entries are the only other dependencies left out.
//!
//! One `mvn`/`mvnw` invocation resolves every library that still has a missing jar,
//! with the user's settings, mirrors, and credentials. Editor refresh does not call
//! this. A library whose jars do not all exist afterwards stays missing.

use std::io;
use std::path::{Path, PathBuf};

use super::model::{ProviderKind, RepositoryLibrary};
use super::runner::{Command, CommandRunner, ProcessRunner, MAVEN, SCAN_PROBE_TIMEOUT};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallResult {
    pub installed: usize,
    pub failed: usize,
}

/// One Maven invocation for every library that still has a missing jar. No repository
/// URL and no global transitive exclusion: a library that includes transitives must
/// be able to install them, and a library that opted out excludes them on its own
/// dependency.
pub fn maven_batch_command(root: &Path, reactor_pom: &Path) -> Command {
    Command::new(MAVEN.resolve(root), root)
        .args([
            "-B".to_string(),
            "--quiet".to_string(),
            "-fae".to_string(),
            "-f".to_string(),
            reactor_pom.to_string_lossy().into_owned(),
            "dependency:resolve".to_string(),
        ])
        .with_timeout(SCAN_PROBE_TIMEOUT)
}

fn write_resolution_reactor(
    directory: &Path,
    libraries: &[&RepositoryLibrary],
) -> io::Result<usize> {
    std::fs::create_dir_all(directory)?;
    let mut modules = String::new();
    for (index, library) in libraries.iter().enumerate() {
        let name = format!("m{index}");
        let module_dir = directory.join(&name);
        std::fs::create_dir_all(&module_dir)?;
        std::fs::write(module_dir.join("pom.xml"), module_pom(index, library))?;
        modules.push_str(&format!("    <module>{name}</module>\n"));
    }
    std::fs::write(directory.join("pom.xml"), aggregator_pom(&modules))?;
    Ok(libraries.len())
}

fn module_pom(index: usize, library: &RepositoryLibrary) -> String {
    let mut exclusions = String::new();
    if !library.include_transitive {
        exclusions.push_str(
            "        <exclusion>\n          <groupId>*</groupId>\n          <artifactId>*</artifactId>\n        </exclusion>\n",
        );
    }
    for excluded in &library.excluded {
        exclusions.push_str(&format!(
            "        <exclusion>\n          <groupId>{}</groupId>\n          <artifactId>{}</artifactId>\n        </exclusion>\n",
            xml_text(&excluded.group),
            xml_text(&excluded.artifact),
        ));
    }
    let exclusions = if exclusions.is_empty() {
        String::new()
    } else {
        format!("      <exclusions>\n{exclusions}      </exclusions>\n")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>krusty.resolution</groupId>
  <artifactId>library-{index}</artifactId>
  <version>1</version>
  <dependencies>
    <dependency>
      <groupId>{group}</groupId>
      <artifactId>{artifact}</artifactId>
      <version>{version}</version>
      <type>{packaging}</type>
{exclusions}    </dependency>
  </dependencies>
</project>
"#,
        group = xml_text(&library.group),
        artifact = xml_text(&library.artifact),
        version = xml_text(&library.version),
        packaging = xml_text(&library.packaging),
    )
}

fn aggregator_pom(modules: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>krusty.resolution</groupId>
  <artifactId>repository-libraries</artifactId>
  <version>1</version>
  <packaging>pom</packaging>
  <modules>
{modules}  </modules>
</project>
"#
    )
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// JPS only. Gradle and Maven projects keep the classpath their own probe returned.
/// Editor refresh does not call this.
pub fn ensure_repository_libraries(
    kind: ProviderKind,
    root: &Path,
    libraries: &[RepositoryLibrary],
) -> InstallResult {
    resolve_missing_libraries(kind, libraries, |libraries| {
        maven_resolve_all(root, libraries)
    })
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

fn maven_resolve_all(root: &Path, libraries: &[&RepositoryLibrary]) -> io::Result<()> {
    let directory = std::env::temp_dir().join(format!(
        "krusty-maven-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    let _cleanup = RemoveDir(directory.clone());
    let written = write_resolution_reactor(&directory, libraries)?;
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn a_library_with_several_artifacts_is_one_dependency_and_keeps_transitives() {
        let tree = crate::project::testing::TempTree::new("maven-library-reactor");
        tree.write("mvnw", "#!/bin/sh\n");
        let repository = tree.directory("m2");
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

        let reactor = tree.directory("reactor");
        let libraries = [&logback, &dexlib];
        assert_eq!(write_resolution_reactor(&reactor, &libraries).unwrap(), 2);
        let aggregator = std::fs::read_to_string(reactor.join("pom.xml")).unwrap();
        assert!(aggregator.contains("<module>m0</module>"), "{aggregator}");
        assert!(aggregator.contains("<module>m1</module>"), "{aggregator}");
        assert!(
            aggregator.contains("<packaging>pom</packaging>"),
            "{aggregator}"
        );

        let with_transitives = std::fs::read_to_string(reactor.join("m0/pom.xml")).unwrap();
        assert_eq!(
            with_transitives.matches("<dependency>").count(),
            1,
            "{with_transitives}"
        );
        assert!(
            with_transitives.contains("<artifactId>logback-classic</artifactId>"),
            "{with_transitives}"
        );
        assert!(
            !with_transitives.contains("logback-core"),
            "{with_transitives}"
        );
        assert!(
            !with_transitives.contains("<groupId>*</groupId>"),
            "{with_transitives}"
        );
        assert!(
            with_transitives.contains("<artifactId>slf4j-api</artifactId>"),
            "{with_transitives}"
        );
        assert!(
            with_transitives.contains("<artifactId>javax.mail</artifactId>"),
            "{with_transitives}"
        );

        let without_transitives = std::fs::read_to_string(reactor.join("m1/pom.xml")).unwrap();
        assert_eq!(
            without_transitives.matches("<dependency>").count(),
            1,
            "{without_transitives}"
        );
        assert!(
            without_transitives.contains("<artifactId>smali-dexlib2</artifactId>"),
            "{without_transitives}"
        );
        assert!(
            without_transitives.contains("<groupId>*</groupId>"),
            "{without_transitives}"
        );
        assert!(
            without_transitives.contains("<artifactId>*</artifactId>"),
            "{without_transitives}"
        );
        assert!(
            !aggregator.contains("http")
                && !with_transitives.contains("http")
                && !without_transitives.contains("curl"),
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
            ]
        );
        assert!(command.args.iter().all(|arg| {
            !arg.contains("http")
                && !arg.contains("curl")
                && !arg.contains("dependency:get")
                && !arg.contains("excludeTransitive")
        }));
        assert_eq!(command.timeout, SCAN_PROBE_TIMEOUT);
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
