//! The JDK homes installed on this machine, by feature release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// JDK homes by feature release. Installed JDKs are found where the platform puts them
/// (`/usr/lib/jvm`, `/Library/Java/JavaVirtualMachines`), then `JAVA_HOME`, then the
/// `<cache>/<release>` homes `scripts/provision-jdks.sh` installs (`KRUSTY_JDK_CACHE`, else
/// `target/cache/jdks`), then the `JAVA_HOME_<release>_X64` variables `actions/setup-java` exports,
/// then `KRUSTY_JDK_<release>_HOME`; a later source overrides an earlier one. Each home's own
/// `release` file says its release.
pub fn available_jdks(releases: &[u32]) -> BTreeMap<u32, PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    for parent in ["/usr/lib/jvm", "/Library/Java/JavaVirtualMachines"] {
        if let Ok(entries) = std::fs::read_dir(parent) {
            let mut found: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .map(|path| {
                    let mac = path.join("Contents/Home");
                    if mac.is_dir() {
                        mac
                    } else {
                        path
                    }
                })
                .collect();
            found.sort();
            homes.extend(found);
        }
    }
    homes.extend(std::env::var_os("JAVA_HOME").map(PathBuf::from));
    let cache = std::env::var_os("KRUSTY_JDK_CACHE")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cache/jdks"));
    homes.extend(
        releases
            .iter()
            .map(|release| cache.join(release.to_string())),
    );
    for release in releases {
        for variable in [
            format!("JAVA_HOME_{release}_X64"),
            format!("JAVA_HOME_{release}_ARM64"),
            format!("KRUSTY_JDK_{release}_HOME"),
        ] {
            homes.extend(std::env::var_os(variable).map(PathBuf::from));
        }
    }
    let mut jdks = BTreeMap::new();
    for home in homes {
        if let Some(release) = jdk_release(&home) {
            jdks.insert(release, home);
        }
    }
    jdks
}

/// The feature release a JDK home's `release` file declares.
pub fn jdk_release(home: &Path) -> Option<u32> {
    release_of_record(&std::fs::read_to_string(home.join("release")).ok()?)
}

/// The feature release a JDK `release` record declares (`JAVA_VERSION="17.0.2"` → 17,
/// `"1.8.0_402"` → 8), or `None` when the record names no version.
pub fn release_of_record(record: &str) -> Option<u32> {
    let version = record
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?
        .trim_matches('"');
    let mut parts = version.split(['.', '_', '+', '-']);
    match parts.next()?.parse().ok()? {
        1 => parts.next()?.parse().ok(),
        release => Some(release),
    }
}

/// The JDK home the runner JVMs, javac, and the reference kotlinc server run on:
/// `KRUSTY_REF_JAVA_HOME`, else `JAVA_HOME`. `None` when neither names a home.
pub fn selected_java_home() -> Option<String> {
    std::env::var("KRUSTY_REF_JAVA_HOME")
        .or_else(|_| std::env::var("JAVA_HOME"))
        .ok()
        .filter(|v| !v.is_empty())
}

/// [`selected_java_home`], which every JVM-backed test requires.
pub fn java_home() -> String {
    selected_java_home().unwrap_or_else(|| {
        panic!(
            "JAVA_HOME is not set, so no JVM-backed test in this suite can run.\n\
             There is no fallback to /usr/libexec/java_home — set it explicitly to a JDK 21+ home."
        )
    })
}
