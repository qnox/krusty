//! Shared test helpers.

use std::path::PathBuf;

use krusty::jvm::classpath::Classpath;

/// Locate a complete kotlin-stdlib jar from standard local caches, mirroring how a drop-in
/// `kotlinc` user supplies it via `-classpath`. "Complete" = the jar's `TypeAliasesKt` facades
/// yield type aliases when scanned (so `Exception` etc. resolve). Returns `None` if none is found,
/// in which case an exception-using test should skip (like a missing `JAVA_HOME`).
#[allow(dead_code)]
pub fn stdlib_jar() -> Option<PathBuf> {
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

fn collect_stdlib_jars(dir: &std::path::Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
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
