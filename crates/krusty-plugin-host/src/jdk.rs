//! Finding a JDK that is already installed. A JDK home names its feature release in its `release`
//! file, so a candidate is judged without starting it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::environment::Platform;

/// A usable JDK: its home and its `java` launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Jdk {
    pub home: PathBuf,
    pub java: PathBuf,
    pub major: u32,
}

/// The installed JDKs to consider, in order, and how each was found.
pub(crate) fn candidates(
    java_home: Option<&Path>,
    path: Option<&OsStr>,
    platform: &Platform,
) -> Vec<(PathBuf, &'static str)> {
    let mut found = Vec::new();
    if let Some(home) = java_home {
        found.push((home.to_path_buf(), "JAVA_HOME"));
    }
    if let Some(home) = path.and_then(|path| home_of_java_on_path(path, platform)) {
        if !found.iter().any(|(known, _)| *known == home) {
            found.push((home, "PATH"));
        }
    }
    found
}

/// Reads a JDK home, or says why it is not one.
pub(crate) fn inspect(home: &Path, platform: &Platform) -> Result<Jdk, String> {
    let java = launcher(home, platform);
    if !java.is_file() {
        return Err(format!(
            "{} has no {}",
            home.display(),
            relative_launcher(platform)
        ));
    }
    let release = std::fs::read_to_string(home.join("release")).map_err(|_| {
        format!(
            "{} has no `release` file naming its Java version",
            home.display()
        )
    })?;
    let major = release_major(&release)
        .ok_or_else(|| format!("{}/release names no JAVA_VERSION", home.display()))?;
    Ok(Jdk {
        home: home.to_path_buf(),
        java,
        major,
    })
}

pub(crate) fn launcher(home: &Path, platform: &Platform) -> PathBuf {
    home.join(relative_launcher(platform))
}

fn relative_launcher(platform: &Platform) -> &'static str {
    if platform.is_windows() {
        "bin/java.exe"
    } else {
        "bin/java"
    }
}

/// The JDK home of the first `java` on `PATH`, following symbolic links such as Debian's
/// `/usr/bin/java` → `/usr/lib/jvm/<jdk>/bin/java`. A Java 8 `jre/bin/java` belongs to the JDK
/// above its `jre`.
fn home_of_java_on_path(path: &OsStr, platform: &Platform) -> Option<PathBuf> {
    let name = if platform.is_windows() {
        "java.exe"
    } else {
        "java"
    };
    let java = std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|java| java.is_file())?;
    let java = std::fs::canonicalize(&java).unwrap_or(java);
    let home = java.parent()?.parent()?;
    if home.file_name().is_some_and(|name| name == "jre") && !home.join("release").exists() {
        return home.parent().map(Path::to_path_buf);
    }
    Some(home.to_path_buf())
}

/// The feature release in a `release` file's `JAVA_VERSION="…"`: `21` for `21.0.8`, `8` for
/// `1.8.0_402`.
fn release_major(release: &str) -> Option<u32> {
    let version = release.lines().find_map(|line| {
        let value = line.strip_prefix("JAVA_VERSION=")?;
        Some(value.trim().trim_matches('"'))
    })?;
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_files_name_their_feature_release() {
        assert_eq!(
            release_major("IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"21.0.8\"\n"),
            Some(21)
        );
        assert_eq!(release_major("JAVA_VERSION=\"1.8.0_402\""), Some(8));
        assert_eq!(release_major("JAVA_VERSION=\"17\""), Some(17));
        assert_eq!(release_major("JAVA_VERSION=\"25-ea\""), Some(25));
        assert_eq!(release_major("JAVA_RUNTIME_VERSION=\"21\""), None);
    }
}
