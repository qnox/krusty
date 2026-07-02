//! Locating the Kotlin toolchain jars a faithful drop-in `kotlinc` compiles against: the
//! kotlin-stdlib family (stdlib + test + reflect + jdk8 + coroutines + annotations) and the JDK
//! `lib/modules` bootclasspath jimage. This is the SINGLE source of truth shared by the test harness
//! (`tests/common`) and the box-corpus `survey` binary, so both build the **same** `-classpath` the
//! conformance gate does — a survey run can't drift from the gate by reimplementing jar location.
//!
//! Jars are taken, in order of fidelity: the reference `kotlinc` dist `lib/` (the exact jars the
//! gate runs, located via `KRUSTY_KOTLINC`), then the local Gradle/Maven caches, then a download
//! from Maven Central (cached under `~/.cache/krusty-deps`). Each is optional — a missing jar just
//! (correctly) leaves the cases needing it blocked, never falsely blocks the rest.

use crate::conformance::directive;
use crate::jvm::classpath::Classpath;
use std::path::PathBuf;

/// Locate a complete kotlin-stdlib jar from the dist or standard local caches, mirroring how a
/// drop-in `kotlinc` user supplies it via `-classpath`. "Complete" = the jar's facades yield type
/// aliases when scanned (so `Exception` etc. resolve). `None` if none is found.
pub fn stdlib_jar() -> Option<PathBuf> {
    // Prefer the dist's own stdlib — the exact jar the reference compiler uses.
    if let Some(j) = dist_jar("kotlin-stdlib.jar") {
        let cp = Classpath::new(vec![j.clone()]);
        if !cp.scan_types().type_aliases.is_empty() {
            return Some(j);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let roots = [
        format!("{home}/.gradle"),
        format!("{home}/.m2/repository/org/jetbrains/kotlin"),
    ];
    let mut found = Vec::new();
    for r in &roots {
        collect_stdlib_jars(std::path::Path::new(r), &mut found, 0);
    }
    // Prefer a jar whose scan actually yields aliases (a real, non-stub stdlib).
    for jar in found {
        let cp = Classpath::new(vec![jar.clone()]);
        if !cp.scan_types().type_aliases.is_empty() {
            return Some(jar);
        }
    }
    None
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

/// The `lib/` dir of the kotlinc dist we differential-test against (`KRUSTY_KOTLINC` points at its
/// `bin/kotlinc`). Its jars are the **exact** ones the reference compiler ships — the truest match,
/// so we prefer them over Maven/gradle copies. `None` when `KRUSTY_KOTLINC` is unset.
pub fn kotlinc_lib_dir() -> Option<PathBuf> {
    let kc = std::env::var("KRUSTY_KOTLINC")
        .ok()
        .filter(|s| !s.is_empty())?;
    let lib = PathBuf::from(kc).parent()?.parent()?.join("lib");
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
/// → `1.9.24`) or a located versioned stdlib jar, defaulting to a known-good version.
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
        .unwrap_or_else(|| "2.0.21".to_string())
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
    let status = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "60", "-o"])
        .arg(&file)
        .arg(&url)
        .status()
        .ok()?;
    if status.success() && file.is_file() {
        Some(file)
    } else {
        let _ = std::fs::remove_file(&file);
        None
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
    dist_jar("kotlin-test.jar")
        .or_else(|| find_jar("kotlin-test-", &["junit", "testng", "annotations"]))
        .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-test", &kotlin_version()))
}

/// The JDK `lib/modules` jimage (the bootclasspath the front-end resolves `java.*` against). Explicit
/// `KRUSTY_SURVEY_JDK_MODULES` override, else derived from `JAVA_HOME`/`KRUSTY_REF_JAVA_HOME`.
pub fn jdk_modules() -> Option<PathBuf> {
    if let Some(p) = std::env::var("KRUSTY_SURVEY_JDK_MODULES")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
    {
        return p.is_file().then_some(p);
    }
    let home = std::env::var("JAVA_HOME")
        .or_else(|_| std::env::var("KRUSTY_REF_JAVA_HOME"))
        .ok()?;
    let p = PathBuf::from(home).join("lib").join("modules");
    p.is_file().then_some(p)
}

fn collect_stdlib_jars(dir: &std::path::Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_stdlib_jars(&p, out, depth + 1);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("kotlin-stdlib-")
                && name.ends_with(".jar")
                && !name.contains("sources")
                && !name.contains("javadoc")
                && !name.contains("common")
                && !name.contains("-js")
                && !name.contains("wasm")
            {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh, uniquely-named temp directory (no external tempfile dependency). Cleaned by the caller.
    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krusty_toolchain_test_{}_{}",
            std::process::id(),
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn collect_named_jars_filters_by_prefix_ext_and_excludes() {
        let dir = temp_dir();
        touch(&dir, "kotlin-test-1.9.24.jar"); // match
        touch(&dir, "kotlin-test-junit-1.9.jar"); // excluded via `excludes`
        touch(&dir, "kotlin-test-sources.jar"); // bad: sources
        touch(&dir, "kotlin-test-1.9.txt"); // not a .jar
        touch(&dir, "kotlin-reflect-1.9.jar"); // wrong prefix

        let mut out = Vec::new();
        collect_named_jars(&dir, "kotlin-test-", &["junit"], &mut out, 0);
        assert_eq!(names(&out), vec!["kotlin-test-1.9.24.jar".to_string()]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_named_jars_recurses_into_subdirs() {
        let dir = temp_dir();
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        touch(&sub, "kotlin-test-2.0.0.jar");

        let mut out = Vec::new();
        collect_named_jars(&dir, "kotlin-test-", &[], &mut out, 0);
        assert_eq!(names(&out), vec!["kotlin-test-2.0.0.jar".to_string()]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_named_jars_respects_depth_guard() {
        let dir = temp_dir();
        touch(&dir, "kotlin-test-1.9.jar");

        // depth > 9 returns immediately without scanning.
        let mut out = Vec::new();
        collect_named_jars(&dir, "kotlin-test-", &[], &mut out, 10);
        assert!(out.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_named_jars_stops_when_full() {
        let dir = temp_dir();
        touch(&dir, "kotlin-test-1.9.jar");

        // out already over the cap (> 8) -> the entry guard bails without adding more.
        let mut out: Vec<PathBuf> = (0..9).map(|i| PathBuf::from(format!("x{i}"))).collect();
        collect_named_jars(&dir, "kotlin-test-", &[], &mut out, 0);
        assert_eq!(out.len(), 9);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_stdlib_jars_filters_variants() {
        let dir = temp_dir();
        touch(&dir, "kotlin-stdlib-1.9.24.jar"); // match
        touch(&dir, "kotlin-stdlib-common-1.9.jar"); // bad: common
        touch(&dir, "kotlin-stdlib-sources.jar"); // bad: sources
        touch(&dir, "kotlin-stdlib-js-1.9.jar"); // bad: -js
        touch(&dir, "kotlin-stdlib.jar"); // no trailing dash -> wrong prefix
        touch(&dir, "kotlin-reflect-1.9.jar"); // wrong prefix

        let mut out = Vec::new();
        collect_stdlib_jars(&dir, &mut out, 0);
        assert_eq!(names(&out), vec!["kotlin-stdlib-1.9.24.jar".to_string()]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn collect_stdlib_jars_respects_depth_guard() {
        let dir = temp_dir();
        touch(&dir, "kotlin-stdlib-1.9.jar");

        let mut out = Vec::new();
        collect_stdlib_jars(&dir, &mut out, 9); // depth > 8 -> immediate return
        assert!(out.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
