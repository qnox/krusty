//! Provisioning against a fake repository: what is reused, what is fetched, and what the user is told.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use krusty_plugin_host::{
    provision, ArtifactPin, Environment, Fetcher, HostRuntime, JdkPin, Pins, Platform,
    ProvisionError, Request,
};
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "https://repo.test/maven2";
const JDK_URL: &str = "https://jdk.test/jdk-21-test.tar.gz";

/// Serves fixed bytes per URL and records what was asked for.
#[derive(Default)]
struct FakeRepository {
    files: HashMap<String, Vec<u8>>,
    fetched: RefCell<Vec<String>>,
}

impl Fetcher for FakeRepository {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        self.fetched.borrow_mut().push(url.to_string());
        let bytes = self
            .files
            .get(url)
            .ok_or("The requested URL returned error: 404")?;
        std::fs::write(dest, bytes).map_err(|error| error.to_string())
    }
}

struct Fixture {
    root: PathBuf,
    env: Environment,
    pins: Pins,
    repository: FakeRepository,
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn artifact(name: &str, content: &[u8]) -> ArtifactPin {
    ArtifactPin {
        group: "org.example".into(),
        artifact: name.into(),
        version: "1.0".into(),
        sha256: sha256(content),
        size: content.len() as u64,
    }
}

fn jdk_home(dir: &Path, version: &str) -> PathBuf {
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/java"), "").unwrap();
    std::fs::write(dir.join("release"), format!("JAVA_VERSION=\"{version}\"\n")).unwrap();
    dir.to_path_buf()
}

/// A gzip tarball holding `jdk-21-test/bin/java` and its `release` file, made with the system `tar`
/// the way Temurin's archives are laid out.
fn jdk_archive(root: &Path) -> Vec<u8> {
    let staging = root.join("archive");
    jdk_home(&staging.join("jdk-21-test"), "21.0.8");
    let archive = root.join("jdk.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .arg("jdk-21-test")
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(archive).unwrap()
}

fn fixture(name: &str) -> Fixture {
    let root =
        std::env::temp_dir().join(format!("krusty-plugin-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let archive = jdk_archive(&root);
    let compiler = artifact("compiler", b"compiler jar");
    let runtime = artifact("runtime", b"runtime jar");
    let jdk = JdkPin {
        release: "test-21".into(),
        major: 21,
        url: JDK_URL.into(),
        sha256: sha256(&archive),
        size: archive.len() as u64,
        java: PathBuf::from("jdk-21-test/bin/java"),
    };
    let mut repository = FakeRepository::default();
    repository.files.insert(JDK_URL.into(), archive);
    repository.files.insert(
        format!("{REPOSITORY}/org/example/compiler/1.0/compiler-1.0.jar"),
        b"compiler jar".to_vec(),
    );
    repository.files.insert(
        format!("{REPOSITORY}/org/example/runtime/1.0/runtime-1.0.jar"),
        b"runtime jar".to_vec(),
    );
    let env = Environment {
        platform: Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        java_home: None,
        path: None,
        gradle_user_home: Some(root.join("gradle")),
        maven_local: Some(root.join("m2")),
        cache_dir: root.join("cache"),
        maven_repository: REPOSITORY.into(),
        offline: false,
    };
    let pins = Pins::new(Some(jdk), vec![("2.4.20".into(), vec![compiler, runtime])]);
    Fixture {
        root,
        env,
        pins,
        repository,
    }
}

impl Fixture {
    fn run(&self, min_java: u32) -> (Result<HostRuntime, ProvisionError>, String) {
        let request = Request {
            kotlin_version: "2.4.20",
            reason: "the compiler plugin compose.jar",
            min_java,
        };
        let mut report = Vec::new();
        let result = provision(
            &request,
            &self.env,
            &self.pins,
            &self.repository,
            &mut report,
        );
        let report = String::from_utf8(report).unwrap();
        (
            result,
            report.replace(&self.root.display().to_string(), "<tmp>"),
        )
    }

    fn fetched(&self) -> Vec<String> {
        self.repository.fetched.borrow().clone()
    }

    fn cached_jars(&self) -> Vec<PathBuf> {
        let jars = self.root.join("cache/maven/org/example");
        vec![
            jars.join("compiler/1.0/compiler-1.0.jar"),
            jars.join("runtime/1.0/runtime-1.0.jar"),
        ]
    }

    /// Everything in the cache directory, so a test can show nothing partial was left behind.
    fn cache_files(&self) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.push(path.strip_prefix(root).unwrap().display().to_string());
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &self.root.join("cache"),
            &self.root.join("cache"),
            &mut files,
        );
        files.sort();
        files
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn downloads_what_is_missing_says_why_and_reuses_it_silently() {
    let mut fixture = fixture("download");
    fixture.env.java_home = Some(jdk_home(&fixture.root.join("jdk11"), "11.0.2"));
    let (result, report) = fixture.run(17);
    let java_home = fixture
        .root
        .join("cache/jdk/test-21-linux-x86_64/jdk-21-test");
    let expected = HostRuntime {
        kotlin_version: "2.4.20".into(),
        java: java_home.join("bin/java"),
        java_home,
        compiler_classpath: fixture.cached_jars(),
    };
    assert_eq!(result, Ok(expected.clone()));
    let archive_size = fixture.repository.files[JDK_URL].len();
    assert_eq!(
        report,
        format!(
        "info: the compiler plugin compose.jar runs in a JVM, but no installed JDK is Java 17 or newer \
         (JAVA_HOME <tmp>/jdk11 is Java 11); krusty downloads one\n\
         info: downloading the test-21 JDK for linux-x86_64 ({archive_size} bytes) from https://jdk.test/jdk-21-test.tar.gz\n\
         info: the compiler plugin compose.jar runs on kotlinc's own compiler, kotlin-compiler-embeddable \
         2.4.20, which krusty keeps in its cache\n\
         info: downloading compiler-1.0.jar (12 bytes) from https://repo.test/maven2/org/example/compiler/1.0/compiler-1.0.jar\n\
         info: downloading runtime-1.0.jar (11 bytes) from https://repo.test/maven2/org/example/runtime/1.0/runtime-1.0.jar\n\
         info: saved in <tmp>/cache; later builds reuse it\n"
        )
    );
    assert_eq!(fixture.fetched().len(), 3);
    assert_eq!(
        fixture.cache_files(),
        [
            "jdk/test-21-linux-x86_64/jdk-21-test/bin/java",
            "jdk/test-21-linux-x86_64/jdk-21-test/release",
            "maven/org/example/compiler/1.0/compiler-1.0.jar",
            "maven/org/example/runtime/1.0/runtime-1.0.jar",
        ]
    );

    let (result, report) = fixture.run(17);
    assert_eq!(result, Ok(expected));
    assert_eq!(report, "");
    assert_eq!(fixture.fetched().len(), 3);
}

#[test]
fn a_new_enough_java_home_is_used_as_it_is() {
    let mut fixture = fixture("java-home");
    let home = jdk_home(&fixture.root.join("jdk17"), "17.0.12");
    fixture.env.java_home = Some(home.clone());
    let (result, _) = fixture.run(17);
    let runtime = result.unwrap();
    assert_eq!(
        (runtime.java, runtime.java_home),
        (home.join("bin/java"), home)
    );
    assert!(!fixture.fetched().contains(&JDK_URL.to_string()));
}

#[cfg(unix)]
#[test]
fn java_on_path_is_followed_to_its_jdk_home() {
    let mut fixture = fixture("path");
    let home = jdk_home(&fixture.root.join("jvm/jdk-21"), "21.0.8");
    let bin = fixture.root.join("usr-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(home.join("bin/java"), bin.join("java")).unwrap();
    fixture.env.path = Some(bin.into_os_string());
    let (result, _) = fixture.run(8);
    assert_eq!(
        result.unwrap().java_home,
        std::fs::canonicalize(&home).unwrap()
    );
    assert!(!fixture.fetched().contains(&JDK_URL.to_string()));
}

#[test]
fn a_verified_jar_is_copied_from_the_gradle_cache_and_a_corrupt_one_is_ignored() {
    let mut fixture = fixture("gradle");
    fixture.env.java_home = Some(jdk_home(&fixture.root.join("jdk21"), "21"));
    let gradle = fixture
        .root
        .join("gradle/caches/modules-2/files-2.1/org.example");
    let good = gradle.join("compiler/1.0/0a1b/compiler-1.0.jar");
    std::fs::create_dir_all(good.parent().unwrap()).unwrap();
    std::fs::write(&good, b"compiler jar").unwrap();
    let corrupt = fixture
        .root
        .join("m2/org/example/runtime/1.0/runtime-1.0.jar");
    std::fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
    std::fs::write(&corrupt, b"runtime jaX").unwrap();

    let (result, report) = fixture.run(17);
    assert_eq!(result.unwrap().compiler_classpath, fixture.cached_jars());
    assert_eq!(
        report,
        "info: the compiler plugin compose.jar runs on kotlinc's own compiler, kotlin-compiler-embeddable \
         2.4.20, which krusty keeps in its cache\n\
         info: copying compiler-1.0.jar from the Gradle cache \
         (<tmp>/gradle/caches/modules-2/files-2.1/org.example/compiler/1.0/0a1b/compiler-1.0.jar)\n\
         info: downloading runtime-1.0.jar (11 bytes) from https://repo.test/maven2/org/example/runtime/1.0/runtime-1.0.jar\n\
         info: saved in <tmp>/cache; later builds reuse it\n"
    );
    assert_eq!(
        fixture.fetched(),
        [format!(
            "{REPOSITORY}/org/example/runtime/1.0/runtime-1.0.jar"
        )]
    );
}

#[test]
fn a_broken_cache_entry_is_replaced_rather_than_kept() {
    let fixture = fixture("broken-cache");
    let jars = fixture.cached_jars();
    // One the right size with other bytes, one cut short.
    for (jar, content) in [
        (&jars[0], &b"compiler jaX"[..]),
        (&jars[1], &b"runtime"[..]),
    ] {
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(jar, content).unwrap();
    }
    // An interrupted unpack by an older krusty: the directory is there, the launcher is not.
    let stale_jdk = fixture
        .root
        .join("cache/jdk/test-21-linux-x86_64/jdk-21-test");
    std::fs::create_dir_all(&stale_jdk).unwrap();
    std::fs::write(stale_jdk.join("release"), "").unwrap();

    let (result, _) = fixture.run(17);
    let runtime = result.unwrap();
    assert_eq!(runtime.java, stale_jdk.join("bin/java"));
    assert_eq!(runtime.compiler_classpath, jars);
    assert_eq!(std::fs::read(&jars[0]).unwrap(), b"compiler jar");
    assert_eq!(std::fs::read(&jars[1]).unwrap(), b"runtime jar");
    assert_eq!(
        std::fs::read_to_string(stale_jdk.join("release")).unwrap(),
        "JAVA_VERSION=\"21.0.8\"\n"
    );
    assert_eq!(fixture.fetched().len(), 3);
}

#[test]
fn a_download_that_is_not_the_pinned_file_is_refused_and_not_kept() {
    let mut fixture = fixture("corrupt");
    fixture.env.java_home = Some(jdk_home(&fixture.root.join("jdk21"), "21"));
    let url = format!("{REPOSITORY}/org/example/compiler/1.0/compiler-1.0.jar");
    fixture
        .repository
        .files
        .insert(url.clone(), b"compiler jaX".to_vec());
    let (result, _) = fixture.run(17);
    let error = result.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "the download from {url} is not the file krusty pins (SHA-256 {}, expected {}); nothing \
             was kept",
            sha256(b"compiler jaX"),
            sha256(b"compiler jar")
        )
    );
    assert_eq!(fixture.cache_files(), Vec::<String>::new());
}

#[test]
fn offline_names_everything_that_is_missing_and_downloads_nothing() {
    let mut fixture = fixture("offline");
    fixture.env.offline = true;
    let (result, report) = fixture.run(17);
    assert_eq!(
        result.unwrap_err().to_string(),
        "the compiler plugin compose.jar needs files that are neither installed nor cached, and \
         downloads are off (KRUSTY_OFFLINE): a Java 17 or newer JDK (JAVA_HOME is unset and no java is \
         on PATH), compiler-1.0.jar, runtime-1.0.jar. Run once with downloads on to cache them, or set \
         JAVA_HOME to a Java 17 or newer JDK"
    );
    assert_eq!(report, "");
    assert_eq!(fixture.fetched(), Vec::<String>::new());
}

#[test]
fn an_unpinned_kotlin_version_is_refused_before_anything_is_fetched() {
    let fixture = fixture("unpinned");
    let request = Request {
        kotlin_version: "2.4.20-RC",
        reason: "the compiler plugin compose.jar",
        min_java: 8,
    };
    let result = provision(
        &request,
        &fixture.env,
        &fixture.pins,
        &fixture.repository,
        &mut Vec::new(),
    );
    assert_eq!(
        result.unwrap_err().to_string(),
        "krusty cannot host compiler plugins for Kotlin 2.4.20-RC: it has no pinned \
         kotlin-compiler-embeddable classpath for that exact version (pinned: 2.4.20)"
    );
    assert_eq!(fixture.fetched(), Vec::<String>::new());
}

#[test]
fn a_platform_without_a_pinned_jdk_asks_for_java_home() {
    let mut fixture = fixture("platform");
    fixture.pins = Pins::new(None, vec![("2.4.20".into(), Vec::new())]);
    fixture.env.platform = Platform {
        os: "freebsd".into(),
        arch: "x86_64".into(),
    };
    let (result, _) = fixture.run(17);
    assert_eq!(
        result.unwrap_err().to_string(),
        "the compiler plugin compose.jar runs in a JVM, but no installed JDK is Java 17 or newer \
         (JAVA_HOME is unset and no java is on PATH), and krusty has no JDK to download for \
         freebsd-x86_64. Set JAVA_HOME to a Java 17 or newer JDK"
    );
}

/// The real thing: Maven Central and Adoptium, through the system `curl`.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20 and its classpath"]
fn provisions_the_pinned_kotlin_2_4_20_classpath() {
    let mut env = Environment::from_process().unwrap();
    let cache =
        std::env::temp_dir().join(format!("krusty-plugin-host-real-{}", std::process::id()));
    env.cache_dir = cache.clone();
    let pins = Pins::builtin(&env.platform);
    let request = Request {
        kotlin_version: "2.4.20",
        reason: "the network test",
        min_java: 8,
    };
    let mut report = Vec::new();
    let result = provision(
        &request,
        &env,
        &pins,
        &krusty_plugin_host::Curl,
        &mut report,
    );
    let _ = std::fs::remove_dir_all(&cache);
    eprint!("{}", String::from_utf8_lossy(&report));
    let runtime = result.unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(runtime.compiler_classpath.len(), 8);
}
