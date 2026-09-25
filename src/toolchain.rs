//! Locating the Kotlin toolchain jars a faithful drop-in `kotlinc` compiles against: the
//! kotlin-stdlib family (stdlib + test + reflect + jdk8 + coroutines + annotations) and the JDK
//! `lib/modules` bootclasspath jimage. This is the SINGLE source of truth shared by the test harness
//! (`tests/common`) and the box-corpus `survey` binary, so both build the **same** `-classpath` the
//! conformance gate does — a survey run can't drift from the gate by reimplementing jar location.
//!
//! Jars are taken, in order of fidelity: the reference `kotlinc` dist `lib/` (the exact jars the
//! gate runs, located via the provisioned `target/cache` dist), then the local Gradle/Maven caches, then a download
//! from Maven Central (cached under `~/.cache/krusty-deps`). Each is optional — a missing jar just
//! (correctly) leaves the cases needing it blocked, never falsely blocks the rest.

use crate::conformance::directive;
use crate::jvm::classpath::Classpath;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Locate a complete kotlin-stdlib jar from the dist or standard local caches, mirroring how a
/// drop-in `kotlinc` user supplies it via `-classpath`. "Complete" = the jar's facades yield type
/// aliases when scanned (so `Exception` etc. resolve). `None` if none is found.
pub fn stdlib_jar() -> Option<PathBuf> {
    // Prefer the dist's own stdlib — the exact jar the reference compiler uses.
    if let Some(j) = dist_jar("kotlin-stdlib.jar") {
        let cp = Classpath::new(vec![j.clone()]);
        if !cp.scan_types().is_empty() {
            return Some(j);
        }
    }
    let version = reference_version();
    let jar = find_exact_dependency_jar("kotlin-stdlib", &version)
        .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-stdlib", &version))?;
    let cp = Classpath::new(vec![jar.clone()]);
    (!cp.scan_types().is_empty()).then_some(jar)
}

/// A `Classpath` containing the located stdlib jar, or empty if none was found.
pub fn stdlib_classpath() -> Classpath {
    match stdlib_jar() {
        Some(j) => Classpath::new(vec![j]),
        None => Classpath::empty(),
    }
}

/// Locate the newest jar whose file name starts with `prefix` and ends with `.jar`, excluding
/// source/javadoc/other-target variants and any of `excludes` substrings.
pub fn find_jar(prefix: &str, excludes: &[&str]) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let roots = [
        format!("{home}/.gradle"),
        format!("{home}/.m2/repository/org/jetbrains"),
    ];
    let mut found = Vec::new();
    for r in &roots {
        collect_named_jars(std::path::Path::new(r), prefix, excludes, &mut found, 0);
    }
    // Prefer the shortest name (the plain `<prefix><version>.jar`, not `-junit`/`-jvm`/…).
    found.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.len())
            .unwrap_or(usize::MAX)
    });
    found.into_iter().next()
}

fn collect_named_jars(
    dir: &std::path::Path,
    prefix: &str,
    excludes: &[&str],
    out: &mut Vec<PathBuf>,
    depth: usize,
) {
    if depth > 9 || out.len() > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_named_jars(&p, prefix, excludes, out, depth + 1);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let bad = ["sources", "javadoc", "-js", "wasm", "common", "metadata"];
            if name.starts_with(prefix)
                && name.ends_with(".jar")
                && !bad.iter().any(|b| name.contains(b))
                && !excludes.iter().any(|b| name.contains(b))
            {
                out.push(p);
            }
        }
    }
}

fn nonempty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn jdk_home_from(java_home: Option<OsString>, reference_home: Option<OsString>) -> Option<PathBuf> {
    // Shell parameter expansion treats an empty JAVA_HOME as unset. Keep the library locator on
    // exactly that contract so `run-tests.sh`, integration tests, and survey runs cannot select
    // different boot classpaths solely because the variable is exported with an empty value.
    nonempty_path(java_home).or_else(|| nonempty_path(reference_home))
}

/// The reference version whose provisioned toolchain this process uses: the one it reproduces.
fn reference_version() -> String {
    crate::kotlin_version::target().to_string()
}

fn find_ancestor(start: &Path, mut matches: impl FnMut(&Path) -> bool) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| matches(path))
        .map(Path::to_path_buf)
}

fn workspace_root_from(start: &Path) -> Option<PathBuf> {
    find_ancestor(start, |path| path.join("kotlin-versions").is_file())
}

fn workspace_root() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| workspace_root_from(path.parent()?))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|path| workspace_root_from(&path))
        })
}

fn provisioned_path(root: &Path, kind: &str, version: &str, suffix: &str) -> PathBuf {
    root.join("target")
        .join("cache")
        .join(kind)
        .join(version)
        .join(suffix)
}

fn toolchain_path(env: Option<OsString>, kind: &str, suffix: &str) -> Option<PathBuf> {
    nonempty_path(env).or_else(|| {
        Some(provisioned_path(
            &workspace_root()?,
            kind,
            &reference_version(),
            suffix,
        ))
    })
}

/// The reference Kotlin/JVM compiler used by differential and conformance-oracle runs.
/// `KRUSTY_KOTLINC` overrides the provisioned distribution.
pub fn kotlinc_path() -> Option<PathBuf> {
    let path = toolchain_path(
        std::env::var_os("KRUSTY_KOTLINC"),
        "kotlinc",
        "kotlinc/bin/kotlinc",
    )?;
    path.is_file().then_some(path)
}

/// The `lib/` dir of the reference kotlinc dist we differential-test against — its jars are the
/// exact ones the reference compiler ships. `KRUSTY_KOTLINC` overrides the provisioned dist.
pub fn kotlinc_lib_dir() -> Option<PathBuf> {
    let kc = kotlinc_path()?;
    let lib = kc.parent()?.parent()?.join("lib");
    lib.is_dir().then_some(lib)
}

/// A jar from the kotlinc dist `lib/` by exact (unversioned) file name, e.g. `kotlin-test.jar`. This
/// exact-name lookup is what makes the dist's UNVERSIONED core jars (`kotlin-stdlib.jar`,
/// `kotlin-test.jar`, `kotlin-reflect.jar`) reachable — a versioned-prefix walk would miss them.
pub fn dist_jar(name: &str) -> Option<PathBuf> {
    let p = kotlinc_lib_dir()?.join(name);
    p.is_file().then_some(p)
}

/// The Kotlin version to pin Maven fallbacks to — from the dist `build.txt` (e.g. `1.9.24-release-822`
/// → `1.9.24`) or a located versioned stdlib jar, defaulting to the process-wide reference
/// target. Dependency provisioning must not silently switch to the newest release when the caller
/// explicitly selected an older supported compiler contract.
pub fn kotlin_version() -> String {
    if let Some(lib) = kotlinc_lib_dir() {
        if let Ok(s) = std::fs::read_to_string(lib.parent().unwrap().join("build.txt")) {
            if let Some(v) = s.trim().split('-').next() {
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    stdlib_jar()
        .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .and_then(|n| {
            n.strip_prefix("kotlin-stdlib-")
                .and_then(|s| s.strip_suffix(".jar"))
                .map(String::from)
        })
        .unwrap_or_else(reference_version)
}

/// The provisioned Kotlin codegen/box corpus root. `KRUSTY_KOTLIN_BOX_DIR` overrides the
/// version-selected cache path.
pub fn box_corpus_dir() -> Option<PathBuf> {
    let p = toolchain_path(
        std::env::var_os("KRUSTY_KOTLIN_BOX_DIR"),
        "box-corpus",
        "compiler/testData/codegen/box",
    )?;
    p.is_dir().then_some(p)
}

/// Locate a dependency jar, downloading it from **Maven Central** into a local cache if not already
/// present (so `// WITH_STDLIB` assertions etc. actually have their jars). Returns `None` only if the
/// download fails (offline). Cached under `~/.cache/krusty-deps` (overridable via `KRUSTY_DEPS_CACHE`).
pub fn ensure_maven(group: &str, artifact: &str, version: &str) -> Option<PathBuf> {
    let cache = std::env::var("KRUSTY_DEPS_CACHE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache/krusty-deps"))
        })?;
    let _ = std::fs::create_dir_all(&cache);
    let file = cache.join(format!("{artifact}-{version}.jar"));
    if file.is_file() {
        return Some(file);
    }
    let url = format!(
        "https://repo1.maven.org/maven2/{}/{artifact}/{version}/{artifact}-{version}.jar",
        group.replace('.', "/")
    );
    let download = maven_download_path(&file);
    // Maven Central refuses or drops a connection now and then; retry before reporting the jar as
    // unavailable, because a missing jar silently leaves its library off every test's classpath.
    let status = std::process::Command::new("curl")
        .args([
            "-sfL",
            "--max-time",
            "60",
            "--retry",
            "4",
            "--retry-all-errors",
        ])
        .args(["--retry-delay", "2", "-o"])
        .arg(&download)
        .arg(&url)
        .status()
        .ok()?;
    if status.success() && download.is_file() {
        publish_maven_download(&download, &file)
    } else {
        let _ = std::fs::remove_file(&download);
        None
    }
}

/// A download is private to one caller until it is complete. The test suite has several independent
/// consumers of the same Maven artifact; writing the final path directly lets one consumer open a
/// partially downloaded JAR while another is still filling it.
fn maven_download_path(file: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DOWNLOAD: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_DOWNLOAD.fetch_add(1, Ordering::Relaxed);
    file.with_extension(format!("jar.download-{}-{sequence}", std::process::id()))
}

/// Publish a completed download with one rename, so the shared final path is never partially
/// visible. Another process may win the same download race; on platforms that refuse to replace an
/// existing file, its already-published result is equally usable.
fn publish_maven_download(download: &Path, file: &Path) -> Option<PathBuf> {
    match std::fs::rename(download, file) {
        Ok(()) => Some(file.to_path_buf()),
        Err(_) if file.is_file() => {
            let _ = std::fs::remove_file(download);
            Some(file.to_path_buf())
        }
        Err(_) => {
            let _ = std::fs::remove_file(download);
            None
        }
    }
}

/// The set of `-classpath` jars a box test needs, formed from its directives — mirroring kotlinc's
/// `JvmEnvironmentConfigurator`: stdlib + kotlin-test + annotations are always present (kotlinc only
/// drops stdlib under `-no-stdlib`); `WITH_REFLECT` adds kotlin-reflect; `STDLIB_JDK8` adds
/// kotlin-stdlib-jdk8; `WITH_COROUTINES` adds kotlinx-coroutines-core. Missing jars are fetched from
/// Maven Central. Memoized per directive-signature — locating jars walks the (huge) gradle/m2 caches,
/// so this collapses thousands of filesystem walks into at most a handful.
pub fn classpath_jars_for(src: &str) -> Vec<PathBuf> {
    let sig: u8 = (directive(src, "WITH_STDLIB") as u8)
        | (directive(src, "WITH_RUNTIME") as u8) << 1
        | (directive(src, "WITH_REFLECT") as u8) << 2
        | (directive(src, "STDLIB_JDK8") as u8) << 3
        | (directive(src, "WITH_COROUTINES") as u8) << 4;
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<u8, Vec<PathBuf>>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(v) = cache.lock().unwrap().get(&sig) {
        return v.clone();
    }
    let jars = classpath_jars_uncached(src);
    cache.lock().unwrap().insert(sig, jars.clone());
    jars
}

fn classpath_jars_uncached(src: &str) -> Vec<PathBuf> {
    let mut jars = Vec::new();
    let v = kotlin_version();
    // `kotlinc` always puts kotlin-stdlib on the compile classpath (only `-no-stdlib` removes it), so a
    // faithful drop-in must too — supply it unconditionally. The explicit directives still select the
    // *extra* jars (reflect, jdk8, coroutines) below.
    if let Some(j) = stdlib_jar() {
        jars.push(j);
    }
    if let Some(j) = kotlin_test_jar() {
        jars.push(j);
    }
    if let Some(j) = dist_jar("annotations-13.0.jar")
        .or_else(|| ensure_maven("org.jetbrains", "annotations", "23.0.0"))
    {
        jars.push(j);
    }
    // EXTRA libraries beyond stdlib — selected per directive from the shared `conformance` decision.
    let extra = crate::conformance::extra_libs(src);
    if extra.reflect {
        if let Some(j) = dist_jar("kotlin-reflect.jar")
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-reflect", &v))
        {
            jars.push(j);
        }
    }
    if extra.stdlib_jdk8 {
        if let Some(j) = dist_jar("kotlin-stdlib-jdk8.jar")
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-stdlib-jdk8", &v))
        {
            jars.push(j);
        }
    }
    if extra.coroutines {
        // Coroutines aren't in the dist; fetch the runtime jar from Maven.
        if let Some(j) = ensure_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-coroutines-core-jvm",
            "1.9.0",
        ) {
            jars.push(j);
        }
    }
    jars
}

/// Locate a `kotlin-test` jar (`// WITH_STDLIB` adds it so `kotlin.test.*` resolves), from the dist,
/// local caches, or Maven Central.
pub fn kotlin_test_jar() -> Option<PathBuf> {
    dist_jar("kotlin-test.jar").or_else(|| {
        let version = reference_version();
        find_exact_dependency_jar("kotlin-test", &version)
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-test", &version))
    })
}

/// The pinned kotlinx.serialization runtime version. The compiler plugin ships with the reference
/// distribution, so the runtime must be a version that plugin emits calls against.
pub const SERIALIZATION_VERSION: &str = "1.9.0";

/// `kotlinx-serialization-core-jvm`, provisioned like every other pinned dependency.
///
/// Searching the local Gradle/Maven caches for "the newest jar whose name starts with …" cannot be
/// used here: it sorts names lexicographically (so `1.9` beats `1.10`) and can pair a core jar with a
/// mismatched json jar. A pinned pair is reproducible and version-consistent.
pub fn serialization_core_jar() -> Option<PathBuf> {
    dist_jar("kotlinx-serialization-core-jvm.jar").or_else(|| {
        ensure_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-serialization-core-jvm",
            SERIALIZATION_VERSION,
        )
    })
}

/// `kotlinx-serialization-json-jvm`, pinned to [`SERIALIZATION_VERSION`] alongside the core jar.
pub fn serialization_json_jar() -> Option<PathBuf> {
    dist_jar("kotlinx-serialization-json-jvm.jar").or_else(|| {
        ensure_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-serialization-json-jvm",
            SERIALIZATION_VERSION,
        )
    })
}

/// Locate the coroutines runtime jar used by classpath suspend tests.
pub fn coroutines_jar() -> Option<PathBuf> {
    dist_jar("kotlinx-coroutines-core-jvm.jar").or_else(|| {
        ensure_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-coroutines-core-jvm",
            "1.9.0",
        )
    })
}

/// The JDK `lib/modules` jimage (the bootclasspath the front-end resolves `java.*` against). Explicit
/// `KRUSTY_SURVEY_JDK_MODULES` override, else derived from `JAVA_HOME`/`KRUSTY_REF_JAVA_HOME`.
pub fn jdk_modules() -> Option<PathBuf> {
    if let Some(p) = nonempty_path(std::env::var_os("KRUSTY_SURVEY_JDK_MODULES")) {
        return p.is_file().then_some(p);
    }
    let home = jdk_home_from(
        std::env::var_os("JAVA_HOME"),
        std::env::var_os("KRUSTY_REF_JAVA_HOME"),
    )?;
    let p = home.join("lib").join("modules");
    p.is_file().then_some(p)
}

/// The selected JDK's versioned public-API symbol archive used for a `--release`-style classpath
/// view. An explicit survey bootclasspath remains authoritative; otherwise this returns `ct.sym`
/// from the same JDK home as [`jdk_modules`]. The caller supplies the release separately.
pub fn jdk_symbols() -> Option<PathBuf> {
    if nonempty_path(std::env::var_os("KRUSTY_SURVEY_JDK_MODULES")).is_some() {
        return jdk_modules();
    }
    let home = jdk_home_from(
        std::env::var_os("JAVA_HOME"),
        std::env::var_os("KRUSTY_REF_JAVA_HOME"),
    )?;
    let symbols = home.join("lib").join("ct.sym");
    symbols.is_file().then_some(symbols)
}

fn find_exact_dependency_jar(artifact: &str, version: &str) -> Option<PathBuf> {
    let expected = format!("{artifact}-{version}.jar");
    let home = std::env::var("HOME").ok()?;
    let maven = PathBuf::from(&home)
        .join(".m2/repository/org/jetbrains/kotlin")
        .join(artifact)
        .join(version)
        .join(&expected);
    if maven.is_file() {
        return Some(maven);
    }
    let gradle = PathBuf::from(&home)
        .join(".gradle/caches/modules-2/files-2.1/org.jetbrains.kotlin")
        .join(artifact)
        .join(version);
    find_exact_jar(&gradle, &expected, 0)
}

fn find_exact_jar(dir: &std::path::Path, expected: &str, depth: usize) -> Option<PathBuf> {
    // A Gradle module version contains hash directories one level below this root. Keep the search
    // bounded to that artifact/version instead of walking the user's entire dependency cache.
    if depth > 2 {
        return None;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_exact_jar(&p, expected, depth + 1) {
                return Some(found);
            }
        } else if p.file_name().and_then(|name| name.to_str()) == Some(expected) {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_dependency_lookup_does_not_substitute_an_installed_version() {
        let root = std::env::temp_dir().join(format!(
            "krusty-toolchain-version-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let old = root.join("old");
        let selected = root.join("selected");
        std::fs::create_dir_all(&old).expect("old dependency directory");
        std::fs::create_dir_all(&selected).expect("selected dependency directory");
        std::fs::write(old.join("kotlin-stdlib-2.1.0.jar"), []).expect("old dependency");
        let expected = selected.join("kotlin-stdlib-2.4.20.jar");
        std::fs::write(&expected, []).expect("selected dependency");

        assert_eq!(
            find_exact_jar(&root, "kotlin-stdlib-2.4.20.jar", 0),
            Some(expected)
        );

        std::fs::remove_dir_all(root).expect("remove toolchain lookup fixture");
    }

    #[test]
    fn provisioned_paths_share_versioned_cache_layout() {
        let a = provisioned_path(
            Path::new("/m"),
            "box-corpus",
            "1.9.24",
            "compiler/testData/codegen/box",
        );
        let b = provisioned_path(Path::new("/m"), "kotlinc", "1.9.24", "kotlinc/bin/kotlinc");
        assert_eq!(
            a,
            Path::new("/m/target/cache/box-corpus/1.9.24/compiler/testData/codegen/box")
        );
        assert_eq!(
            b,
            Path::new("/m/target/cache/kotlinc/1.9.24/kotlinc/bin/kotlinc")
        );
    }

    #[test]
    fn ancestor_is_discovered_from_a_nested_path() {
        let nested = Path::new("/workspace/target/gate/deps");
        assert_eq!(
            find_ancestor(nested, |path| path.ends_with("workspace")),
            Some(PathBuf::from("/workspace"))
        );
    }

    #[test]
    fn path_override_is_os_native_and_empty_is_unset() {
        assert_eq!(
            nonempty_path(Some(OsString::from("/toolchain"))),
            Some(PathBuf::from("/toolchain"))
        );
        assert_eq!(nonempty_path(Some(OsString::new())), None);
        assert_eq!(nonempty_path(None), None);
    }

    #[test]
    fn empty_java_home_falls_back_to_reference_jdk() {
        assert_eq!(
            jdk_home_from(
                Some(OsString::new()),
                Some(OsString::from("/reference-jdk"))
            ),
            Some(PathBuf::from("/reference-jdk"))
        );
        assert_eq!(
            jdk_home_from(
                Some(OsString::from("/primary-jdk")),
                Some(OsString::from("/reference-jdk"))
            ),
            Some(PathBuf::from("/primary-jdk"))
        );
    }

    #[test]
    fn maven_download_is_hidden_until_atomic_publication() {
        let directory = std::env::temp_dir().join(format!(
            "krusty-maven-publication-{}-{}",
            std::process::id(),
            maven_download_path(Path::new("sequence.jar"))
                .extension()
                .expect("download suffix")
                .to_string_lossy()
        ));
        std::fs::create_dir(&directory).expect("create isolated Maven cache");
        let file = directory.join("dependency-1.0.jar");
        let download = maven_download_path(&file);
        let concurrent_download = maven_download_path(&file);
        assert_ne!(
            download, concurrent_download,
            "concurrent callers need private paths"
        );
        std::fs::write(&download, b"complete jar").expect("write private download");

        assert!(
            !file.exists(),
            "the final cache path must not expose a partial download"
        );
        assert_eq!(publish_maven_download(&download, &file), Some(file.clone()));
        assert_eq!(std::fs::read(&file).unwrap(), b"complete jar");
        assert!(
            !download.exists(),
            "the private path was renamed, not copied"
        );

        std::fs::remove_dir_all(directory).expect("remove isolated Maven cache");
    }
}
