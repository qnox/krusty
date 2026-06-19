//! Shared test helpers.

use std::path::PathBuf;

use krusty::jvm::classpath::Classpath;

/// Compile Kotlin `src` to `(internal_name, class_bytes)` pairs entirely in-process — the same pipeline
/// (`lex → parse → check → ir_lower → ir_emit`) the conformance harness uses, sharing the process-global
/// classpath caches (type/ext/jimage indexes) across every call. This is dramatically faster than
/// spawning the `krusty` binary once per snippet (each subprocess rebuilds those indexes from scratch).
/// `cp_jars` are the `-classpath` jars; `jdk_modules` is the JDK `lib/modules` jimage (the bootclasspath).
/// Returns `None` on any compile error (an unsupported feature), like the CLI's non-zero exit.
#[allow(dead_code)]
pub fn compile_in_process(
    src: &str,
    stem: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::jvm::names::file_class_name;
    use krusty::resolve::{check_file, collect_signatures_with_cp};

    let mut diags = DiagSink::new();
    let toks = krusty::lexer::lex(src, &mut diags);
    let files = vec![krusty::parser::parse(src, &toks, &mut diags)];
    if diags.has_errors() {
        return None;
    }
    let mut cp_paths: Vec<PathBuf> = cp_jars.to_vec();
    if let Some(p) = jdk_modules {
        cp_paths.push(p.to_path_buf());
    }
    // Reuse one `Classpath` per classpath set on this thread (warm caches across snippets).
    thread_local! {
        static CP: std::cell::RefCell<std::collections::HashMap<Vec<PathBuf>, std::rc::Rc<Classpath>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let cp = CP.with(|c| {
        c.borrow_mut()
            .entry(cp_paths.clone())
            .or_insert_with(|| std::rc::Rc::new(Classpath::new(cp_paths.clone())))
            .clone()
    });
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let file = &files[0];
    let info = check_file(file, &syms, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let facade = file_class_name(stem, file.package.as_deref());
    let ir = krusty::ir_lower::lower_file(file, &info, &syms)?;
    let outputs = krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp)?;
    if outputs.is_empty() {
        None
    } else {
        Some(outputs)
    }
}

/// Locate a complete kotlin-stdlib jar from standard local caches, mirroring how a drop-in
/// `kotlinc` user supplies it via `-classpath`. "Complete" = the jar's `TypeAliasesKt` facades
/// yield type aliases when scanned (so `Exception` etc. resolve). Returns `None` if none is found,
/// in which case an exception-using test should skip (like a missing `JAVA_HOME`).
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn stdlib_classpath() -> Classpath {
    match stdlib_jar() {
        Some(j) => Classpath::new(vec![j]),
        None => Classpath::empty(),
    }
}

/// Whether a box-test directive (`// NAME` …) is present.
#[allow(dead_code)]
pub fn directive(src: &str, name: &str) -> bool {
    src.lines().any(|l| {
        let l = l.trim();
        l.starts_with("//")
            && l.trim_start_matches('/')
                .trim_start()
                .split([' ', ':'])
                .next()
                == Some(name)
    })
}

/// Locate the newest jar whose file name starts with `prefix` and ends with `.jar`, excluding
/// source/javadoc/other-target variants and any of `excludes` substrings.
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn kotlinc_lib_dir() -> Option<PathBuf> {
    let kc = std::env::var("KRUSTY_KOTLINC").ok()?;
    let lib = PathBuf::from(kc).parent()?.parent()?.join("lib");
    lib.is_dir().then_some(lib)
}

/// A jar from the kotlinc dist `lib/` by exact (unversioned) file name, e.g. `kotlin-test.jar`.
#[allow(dead_code)]
pub fn dist_jar(name: &str) -> Option<PathBuf> {
    let p = kotlinc_lib_dir()?.join(name);
    p.is_file().then_some(p)
}

/// The Kotlin version to pin Maven fallbacks to — from the dist `build.txt` (e.g. `1.9.24-release-822`
/// → `1.9.24`) or a located versioned stdlib jar, defaulting to a known-good version.
#[allow(dead_code)]
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
/// download fails (offline). Cached under `~/.cache/krusty-deps`.
#[allow(dead_code)]
pub fn ensure_maven(group: &str, artifact: &str, version: &str) -> Option<PathBuf> {
    let cache = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".cache/krusty-deps"))?;
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
/// `JvmEnvironmentConfigurator`: `WITH_STDLIB`/`WITH_RUNTIME` add kotlin-stdlib + kotlin-test +
/// annotations; `WITH_REFLECT` adds kotlin-reflect; `STDLIB_JDK8` adds kotlin-stdlib-jdk8;
/// `WITH_COROUTINES` adds kotlinx-coroutines-core. Missing jars are fetched from Maven Central.
#[allow(dead_code)]
pub fn classpath_jars_for(src: &str) -> Vec<PathBuf> {
    // The jar locations are constant; only the *directive set* of a file varies. Locating jars walks
    // the (huge) gradle/m2 caches recursively, so memoize per directive-signature — collapsing
    // thousands of ~1s filesystem walks into at most a handful.
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
    // faithful drop-in must too — supply it unconditionally, not just for `// WITH_STDLIB` tests. The
    // explicit directives still select the *extra* jars (reflect, jdk8, coroutines) below.
    let _ = (
        directive(src, "WITH_STDLIB"),
        directive(src, "WITH_RUNTIME"),
    );
    let with_stdlib = true;
    if with_stdlib {
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
    }
    if directive(src, "WITH_REFLECT") {
        if let Some(j) = dist_jar("kotlin-reflect.jar")
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-reflect", &v))
        {
            jars.push(j);
        }
    }
    if directive(src, "STDLIB_JDK8") {
        if let Some(j) = dist_jar("kotlin-stdlib-jdk8.jar")
            .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-stdlib-jdk8", &v))
        {
            jars.push(j);
        }
    }
    if directive(src, "WITH_COROUTINES") {
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

/// Locate a `kotlin-test` jar (`// WITH_STDLIB` adds it so `kotlin.test.*` resolves), from the local
/// caches or Maven Central.
#[allow(dead_code)]
pub fn kotlin_test_jar() -> Option<PathBuf> {
    dist_jar("kotlin-test.jar")
        .or_else(|| find_jar("kotlin-test-", &["junit", "testng", "annotations"]))
        .or_else(|| ensure_maven("org.jetbrains.kotlin", "kotlin-test", &kotlin_version()))
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
