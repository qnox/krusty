//! The exact files a plugin host may run on, each with the SHA-256 and size it must have.

use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::environment::Platform;

const JDK_TABLE: &str = include_str!("pins/jdk.tsv");
const COMPILER_TABLE: &str = include_str!("pins/kotlin-compiler.tsv");
const KSP_TABLE: &str = include_str!("pins/ksp.tsv");

/// The release of the JDK in `pins/jdk.tsv`, as its archives name it.
const JDK_RELEASE: &str = "temurin-21.0.8+9";
/// The Java feature release of that JDK.
const JDK_MAJOR: u32 = 21;

/// A JDK archive for one platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JdkPin {
    /// The release, such as `temurin-21.0.8+9`; it names the JDK's directory in the cache.
    pub release: String,
    /// The Java feature release the JDK provides.
    pub major: u32,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Where `java` is inside the unpacked archive.
    pub java: PathBuf,
}

impl JdkPin {
    pub fn file_name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or(&self.url)
    }
}

/// One jar of a Maven repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactPin {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub sha256: String,
    pub size: u64,
}

impl ArtifactPin {
    /// The pins of `classpath`, coordinates and digests in order, as one SHA-256: whatever is built
    /// against that classpath is valid for this identity and no other.
    pub fn identity(classpath: &[ArtifactPin]) -> String {
        let mut digest = Sha256::new();
        for pin in classpath {
            digest.update(format!(
                "{}:{}:{} {} {}\n",
                pin.group, pin.artifact, pin.version, pin.sha256, pin.size
            ));
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn file_name(&self) -> String {
        format!("{}-{}.jar", self.artifact, self.version)
    }

    /// The jar's path in a Maven repository layout, with `/` separators.
    pub fn repository_path(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.file_name()
        )
    }
}

/// The JDK to download for this platform, the compiler classpath of each Kotlin version a plugin
/// host can run, and the API classpath of each KSP version a KSP host can run.
#[derive(Clone, Debug)]
pub struct Pins {
    jdk: Option<JdkPin>,
    compilers: Vec<(String, Vec<ArtifactPin>)>,
    ksp: Vec<(String, Vec<ArtifactPin>)>,
}

impl Pins {
    pub fn new(jdk: Option<JdkPin>, compilers: Vec<(String, Vec<ArtifactPin>)>) -> Self {
        Pins {
            jdk,
            compilers,
            ksp: Vec::new(),
        }
    }

    /// These pins, with `classpath` as the API classpath of KSP `version`.
    pub fn with_ksp(mut self, version: &str, classpath: Vec<ArtifactPin>) -> Self {
        self.ksp.push((version.to_string(), classpath));
        self
    }

    /// The tables this crate ships, with the JDK for `platform` when one is pinned for it.
    pub fn builtin(platform: &Platform) -> Self {
        let jdk = rows(JDK_TABLE)
            .filter(|row| row[0] == platform.os && row[1] == platform.arch)
            .map(|row| JdkPin {
                release: JDK_RELEASE.to_string(),
                major: JDK_MAJOR,
                url: row[2].to_string(),
                sha256: row[3].to_string(),
                size: row[4].parse().expect("pins/jdk.tsv: size"),
                java: PathBuf::from(row[5]),
            })
            .next();
        Pins {
            jdk,
            compilers: classpaths(COMPILER_TABLE),
            ksp: classpaths(KSP_TABLE),
        }
    }

    pub fn jdk(&self) -> Option<&JdkPin> {
        self.jdk.as_ref()
    }

    /// The compiler classpath pinned for exactly `kotlin_version`. A version is an exact identity:
    /// `2.4.20-RC` is not `2.4.20`, and neither stands in for the other.
    pub fn compiler_classpath(&self, kotlin_version: &str) -> Option<&[ArtifactPin]> {
        self.compilers
            .iter()
            .find(|(version, _)| version == kotlin_version)
            .map(|(_, classpath)| classpath.as_slice())
    }

    pub fn kotlin_versions(&self) -> Vec<String> {
        self.compilers
            .iter()
            .map(|(version, _)| version.clone())
            .collect()
    }

    /// The API classpath pinned for exactly KSP `ksp_version`.
    pub fn ksp_classpath(&self, ksp_version: &str) -> Option<&[ArtifactPin]> {
        self.ksp
            .iter()
            .find(|(version, _)| version == ksp_version)
            .map(|(_, classpath)| classpath.as_slice())
    }

    pub fn ksp_versions(&self) -> Vec<String> {
        self.ksp
            .iter()
            .map(|(version, _)| version.clone())
            .collect()
    }
}

/// A table of classpaths, one row per jar: the version the classpath is for, the jar's
/// `group:artifact:version`, its SHA-256 and its size. Rows of one classpath are adjacent.
fn classpaths(table: &str) -> Vec<(String, Vec<ArtifactPin>)> {
    let mut classpaths: Vec<(String, Vec<ArtifactPin>)> = Vec::new();
    for row in rows(table) {
        let mut coordinate = row[1].split(':');
        let pin = ArtifactPin {
            group: coordinate.next().expect("pins: group").to_string(),
            artifact: coordinate.next().expect("pins: artifact").to_string(),
            version: coordinate.next().expect("pins: version").to_string(),
            sha256: row[2].to_string(),
            size: row[3].parse().expect("pins: size"),
        };
        match classpaths.last_mut() {
            Some((version, classpath)) if version == row[0] => classpath.push(pin),
            _ => classpaths.push((row[0].to_string(), vec![pin])),
        }
    }
    classpaths
}

fn rows(table: &str) -> impl Iterator<Item = Vec<&str>> {
    table
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split('\t').collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported_kotlin_versions() -> Vec<String> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../kotlin-versions");
        std::fs::read_to_string(path)
            .expect("kotlin-versions")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| line.split_whitespace().next().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_pinned_kotlin_versions_are_exactly_the_supported_ones() {
        let pins = Pins::builtin(&Platform::current());
        // Exactly: a pin left behind for a dropped release would keep that release hostable.
        assert_eq!(pins.kotlin_versions(), supported_kotlin_versions());
        for version in pins.kotlin_versions() {
            let classpath = pins.compiler_classpath(&version).unwrap();
            let compiler = &classpath[0];
            assert_eq!(
                (compiler.artifact.as_str(), compiler.version.as_str()),
                ("kotlin-compiler-embeddable", version.as_str())
            );
            assert_eq!(classpath.len(), 8, "{version}");
            for pin in classpath {
                assert_eq!(pin.sha256.len(), 64, "{pin:?}");
                assert!(pin.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{pin:?}");
            }
        }
    }

    #[test]
    fn every_pinned_ksp_version_runs_on_its_own_api_jar() {
        let pins = Pins::builtin(&Platform::current());
        assert_eq!(pins.ksp_versions(), ["2.3.12"]);
        let classpath = pins.ksp_classpath("2.3.12").unwrap();
        let coordinates: Vec<String> = classpath
            .iter()
            .map(|pin| format!("{}:{}:{}", pin.group, pin.artifact, pin.version))
            .collect();
        assert_eq!(
            coordinates,
            [
                "com.google.devtools.ksp:symbol-processing-api:2.3.12",
                "org.jetbrains.kotlin:kotlin-stdlib:2.3.20",
                "org.jetbrains:annotations:13.0",
            ]
        );
        assert!(pins.ksp_classpath("2.3.12-RC").is_none());
    }

    #[test]
    fn a_kotlin_version_matches_only_exactly() {
        let pins = Pins::builtin(&Platform::current());
        assert!(pins.compiler_classpath("2.4.20").is_some());
        assert!(pins.compiler_classpath("2.4.20-RC").is_none());
        assert!(pins.compiler_classpath("2.4").is_none());
    }

    #[test]
    fn every_pinned_platform_has_a_jdk_inside_its_release_directory() {
        for (os, arch, java) in [
            ("linux", "x86_64", "jdk-21.0.8+9/bin/java"),
            ("linux", "aarch64", "jdk-21.0.8+9/bin/java"),
            ("macos", "x86_64", "jdk-21.0.8+9/Contents/Home/bin/java"),
            ("macos", "aarch64", "jdk-21.0.8+9/Contents/Home/bin/java"),
            ("windows", "x86_64", "jdk-21.0.8+9/bin/java.exe"),
            ("windows", "aarch64", "jdk-21.0.8+9/bin/java.exe"),
        ] {
            let platform = Platform {
                os: os.into(),
                arch: arch.into(),
            };
            let jdk = Pins::builtin(&platform).jdk.unwrap();
            assert_eq!(jdk.java, PathBuf::from(java));
            assert_eq!(jdk.sha256.len(), 64);
            assert!(
                jdk.file_name().starts_with("OpenJDK21U-jdk_"),
                "{}",
                jdk.url
            );
        }
        let other = Platform {
            os: "freebsd".into(),
            arch: "x86_64".into(),
        };
        assert_eq!(Pins::builtin(&other).jdk, None);
    }
}
