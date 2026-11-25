//! The JDK homes installed on this machine, by feature release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// JDK homes by feature release. Installed JDKs are found where the platform puts them
/// (`/usr/lib/jvm`, `/Library/Java/JavaVirtualMachines`), then `JAVA_HOME`, then the
/// `JAVA_HOME_<release>_X64` variables `actions/setup-java` exports, then `KRUSTY_JDK_<release>_HOME`;
/// a later source overrides an earlier one. Each home's own `release` file says its release.
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

/// The feature release a JDK's `release` file declares (`JAVA_VERSION="17.0.2"` → 17, `"1.8.0"` → 8).
fn jdk_release(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let version = text
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?
        .trim_matches('"');
    let mut parts = version.split(['.', '_', '+', '-']);
    match parts.next()?.parse().ok()? {
        1 => parts.next()?.parse().ok(),
        release => Some(release),
    }
}
