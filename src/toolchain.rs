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

    #[test]
    fn collect_stdlib_jars_stops_when_full() {
        let dir = temp_dir();
        touch(&dir, "kotlin-stdlib-1.9.jar");

        // out already over the cap (> 4) -> the entry guard bails without scanning.
        let mut out: Vec<PathBuf> = (0..5).map(|i| PathBuf::from(format!("x{i}"))).collect();
        collect_stdlib_jars(&dir, &mut out, 0);
        assert_eq!(out.len(), 5);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- Env-driven helpers -------------------------------------------------
    //
    // These mutate process-wide environment variables, so they serialize on a shared lock and
    // always restore prior values. No network is touched: every path is a temp-dir fixture.

    /// Serialize env-mutating tests against each other (they share process globals).
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set (or, with `None`, remove) an env var, returning its prior value for restoration.
    fn set_env(key: &str, val: Option<&str>) -> Option<std::ffi::OsString> {
        let prev = std::env::var_os(key);
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        prev
    }

    fn restore_env(key: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// Build a fake kotlinc dist skeleton (`<root>/bin/kotlinc`, `<root>/lib/`) and return `root`.
    fn fake_dist() -> PathBuf {
        let root = temp_dir();
        let bin = root.join("bin");
        let lib = root.join("lib");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        touch(&bin, "kotlinc");
        root
    }

    #[test]
    fn kotlinc_lib_dir_derives_lib_from_bin_and_dist_jar_by_name() {
        let _g = env_lock();
        let root = fake_dist();
        let lib = root.join("lib");
        touch(&lib, "kotlin-test.jar");
        let kc = root.join("bin").join("kotlinc");
        let prev = set_env("KRUSTY_KOTLINC", Some(kc.to_str().unwrap()));

        assert_eq!(kotlinc_lib_dir(), Some(lib.clone()));
        // dist_jar resolves an exact (unversioned) name in that lib dir; a missing one is None.
        assert_eq!(
            dist_jar("kotlin-test.jar"),
            Some(lib.join("kotlin-test.jar"))
        );
        assert!(dist_jar("kotlin-absent.jar").is_none());

        restore_env("KRUSTY_KOTLINC", prev);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn kotlinc_lib_dir_none_when_unset_or_empty_or_missing_lib() {
        let _g = env_lock();
        // Empty string is filtered to None.
        let prev = set_env("KRUSTY_KOTLINC", Some(""));
        assert!(kotlinc_lib_dir().is_none());
        // Removed entirely -> None.
        let _ = set_env("KRUSTY_KOTLINC", None);
        assert!(kotlinc_lib_dir().is_none());
        assert!(dist_jar("kotlin-test.jar").is_none());

        // Points at a bin without a sibling lib/ dir -> None (lib.is_dir() false).
        let root = temp_dir();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        touch(&root.join("bin"), "kotlinc");
        let kc = root.join("bin").join("kotlinc");
        set_env("KRUSTY_KOTLINC", Some(kc.to_str().unwrap()));
        assert!(kotlinc_lib_dir().is_none());

        restore_env("KRUSTY_KOTLINC", prev);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn kotlin_version_reads_and_trims_build_txt() {
        let _g = env_lock();
        let root = fake_dist();
        // build.txt sits beside lib/ (dist root); the version is the pre-`-` prefix, trimmed.
        std::fs::write(root.join("build.txt"), "1.9.24-release-822\n").unwrap();
        let kc = root.join("bin").join("kotlinc");
        let prev = set_env("KRUSTY_KOTLINC", Some(kc.to_str().unwrap()));

        assert_eq!(kotlin_version(), "1.9.24");

        // A bare version with no `-` suffix is returned as-is.
        std::fs::write(root.join("build.txt"), "2.0.0\n").unwrap();
        assert_eq!(kotlin_version(), "2.0.0");

        restore_env("KRUSTY_KOTLINC", prev);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn jdk_modules_honors_explicit_override_then_java_home() {
        let _g = env_lock();
        let root = temp_dir();

        // Explicit override to a real file wins.
        let modules = root.join("modules");
        touch(&root, "modules");
        let prev_override = set_env("KRUSTY_SURVEY_JDK_MODULES", Some(modules.to_str().unwrap()));
        let prev_java = set_env("JAVA_HOME", None);
        let prev_ref = set_env("KRUSTY_REF_JAVA_HOME", None);
        assert_eq!(jdk_modules(), Some(modules.clone()));

        // Override pointing at a non-file -> None (does NOT fall through to JAVA_HOME).
        set_env(
            "KRUSTY_SURVEY_JDK_MODULES",
            Some(root.join("nope").to_str().unwrap()),
        );
        assert!(jdk_modules().is_none());

        // No override -> derive `<JAVA_HOME>/lib/modules`.
        set_env("KRUSTY_SURVEY_JDK_MODULES", None);
        let jh = root.join("jdk");
        std::fs::create_dir_all(jh.join("lib")).unwrap();
        touch(&jh.join("lib"), "modules");
        set_env("JAVA_HOME", Some(jh.to_str().unwrap()));
        assert_eq!(jdk_modules(), Some(jh.join("lib").join("modules")));

        restore_env("KRUSTY_SURVEY_JDK_MODULES", prev_override);
        restore_env("JAVA_HOME", prev_java);
        restore_env("KRUSTY_REF_JAVA_HOME", prev_ref);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn find_jar_prefers_shortest_name_under_home() {
        let _g = env_lock();
        let home = temp_dir();
        let gradle = home.join(".gradle").join("caches");
        std::fs::create_dir_all(&gradle).unwrap();
        touch(&gradle, "kotlin-reflect-1.0.jar"); // plain, shortest -> preferred
        touch(&gradle, "kotlin-reflect-1.0-extra.jar"); // longer variant
        touch(&gradle, "kotlin-reflect-1.0-sources.jar"); // bad: sources (skipped)
        touch(&gradle, "kotlin-reflect-1.0-junit.jar"); // excluded via `excludes`

        let prev_home = set_env("HOME", Some(home.to_str().unwrap()));
        let found = find_jar("kotlin-reflect-", &["junit"]).unwrap();
        restore_env("HOME", prev_home);

        assert_eq!(
            found.file_name().unwrap().to_str().unwrap(),
            "kotlin-reflect-1.0.jar"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn stdlib_jar_and_classpath_none_when_nothing_available() {
        let _g = env_lock();
        // No dist and an empty HOME -> no stdlib is located.
        let home = temp_dir();
        let prev_kc = set_env("KRUSTY_KOTLINC", None);
        let prev_home = set_env("HOME", Some(home.to_str().unwrap()));

        assert!(stdlib_jar().is_none());
        // The empty-classpath branch: scanning yields no type aliases.
        assert!(stdlib_classpath().scan_types().type_aliases.is_empty());

        restore_env("KRUSTY_KOTLINC", prev_kc);
        restore_env("HOME", prev_home);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn ensure_maven_returns_cached_jar_without_network() {
        let _g = env_lock();
        let cache = temp_dir();
        // Pre-seed the exact cache file name `ensure_maven` computes -> it short-circuits (no curl).
        std::fs::write(cache.join("annotations-23.0.0.jar"), b"x").unwrap();
        let prev = set_env("KRUSTY_DEPS_CACHE", Some(cache.to_str().unwrap()));

        let got = ensure_maven("org.jetbrains", "annotations", "23.0.0");
        assert_eq!(got, Some(cache.join("annotations-23.0.0.jar")));

        restore_env("KRUSTY_DEPS_CACHE", prev);
        std::fs::remove_dir_all(&cache).unwrap();
    }
}
