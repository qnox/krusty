//! Content fingerprint of the files whose changes invalidate the project model.
//!
//! Hashing content rather than modification times removes the common false trigger where an editor,
//! a formatter, or a branch switch rewrites a build file byte-identically: a 30-second Gradle sync
//! must not run for a save that changed nothing.

use std::fs;
use std::path::{Path, PathBuf};

/// FNV-1a; no dependency, and the input is a few hundred kilobytes of build files at most.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint(u64);

impl Fingerprint {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hasher(u64);

impl Default for Hasher {
    fn default() -> Self {
        Self(FNV_OFFSET)
    }
}

impl Hasher {
    pub fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    pub fn write_str(&mut self, text: &str) {
        self.write(text.as_bytes());
        self.write(&[0]);
    }

    pub fn finish(self) -> Fingerprint {
        Fingerprint(self.0)
    }
}

/// Hash `paths` in the given order together with `salt` (build-tool version, probe-script version).
///
/// A path that cannot be read contributes its name and an "absent" marker, so that creating or
/// deleting a watched file changes the fingerprint just as editing one does.
pub fn fingerprint_files(paths: &[PathBuf], salt: &str) -> Fingerprint {
    let mut hasher = Hasher::default();
    hasher.write_str(salt);
    for path in paths {
        hasher.write_str(&path.to_string_lossy());
        match fs::read(path) {
            Ok(contents) => {
                hasher.write_str("present");
                hasher.write(&contents);
            }
            Err(_) => hasher.write_str("absent"),
        }
    }
    hasher.finish()
}

/// Directories that never contain build configuration and can be large.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    "build",
    "node_modules",
    "out",
    "target",
];

/// Collect the build files under `root` that `matches` accepts, in a deterministic order.
///
/// `buildSrc` and `build-logic` are walked despite the `build` prefix filter: convention plugins
/// living there change the project model without any edit to a `build.gradle` file.
pub fn collect_build_files(
    root: &Path,
    max_depth: usize,
    matches: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, max_depth, matches, &mut found);
    found.sort();
    found
}

fn walk(
    directory: &Path,
    depth_left: usize,
    matches: &dyn Fn(&Path) -> bool,
    found: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if depth_left == 0 || is_skipped(&path) {
                continue;
            }
            walk(&path, depth_left - 1, matches, found);
        } else if matches(&path) {
            found.push(path);
        }
    }
}

fn is_skipped(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    if name == "buildSrc" || name == "build-logic" {
        return false;
    }
    SKIPPED_DIRECTORIES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::testing::TempTree;

    #[test]
    fn identical_content_hashes_equal_and_edits_change_the_hash() {
        let tree = TempTree::new("fingerprint-content");
        tree.write("build.gradle.kts", "dependencies {}\n");
        let paths = vec![tree.path("build.gradle.kts")];

        let first = fingerprint_files(&paths, "gradle-8.7");
        tree.write("build.gradle.kts", "dependencies {}\n");
        assert_eq!(first, fingerprint_files(&paths, "gradle-8.7"));

        tree.write("build.gradle.kts", "dependencies { implementation(x) }\n");
        assert_ne!(first, fingerprint_files(&paths, "gradle-8.7"));
    }

    #[test]
    fn a_missing_file_differs_from_an_empty_one_and_the_salt_participates() {
        let tree = TempTree::new("fingerprint-absent");
        let paths = vec![tree.path("gradle.properties")];
        let absent = fingerprint_files(&paths, "gradle-8.7");

        tree.write("gradle.properties", "");
        let empty = fingerprint_files(&paths, "gradle-8.7");
        assert_ne!(absent, empty);
        assert_ne!(empty, fingerprint_files(&paths, "gradle-8.8"));
    }

    #[test]
    fn build_file_walk_skips_output_directories_but_keeps_convention_plugin_sources() {
        let tree = TempTree::new("fingerprint-walk");
        tree.write("settings.gradle.kts", "");
        tree.write("app/build.gradle.kts", "");
        tree.write("build/generated/build.gradle.kts", "");
        tree.write("buildSrc/build.gradle.kts", "");
        tree.write(".git/hooks/build.gradle.kts", "");

        let found = collect_build_files(&tree.root, 8, &|path| {
            path.extension().is_some_and(|extension| extension == "kts")
        });
        assert_eq!(
            found,
            vec![
                tree.path("app/build.gradle.kts"),
                tree.path("buildSrc/build.gradle.kts"),
                tree.path("settings.gradle.kts"),
            ]
        );
    }
}
