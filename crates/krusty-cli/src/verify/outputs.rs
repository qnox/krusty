//! Comparing the files kotlinc and krusty wrote for one compilation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use krusty::jvm::class_comparison::compare_classes;
use serde::{Deserialize, Serialize};

/// How krusty's output of a compilation differs from kotlinc's. Paths are relative to the output
/// directory, with `/` separators.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputComparison {
    /// The class files kotlinc wrote.
    pub classes: usize,
    /// The class files krusty wrote byte for byte as kotlinc did.
    pub identical_classes: usize,
    /// Files only kotlinc wrote.
    pub missing: Vec<String>,
    /// Files only krusty wrote.
    pub extra: Vec<String>,
    /// Files both wrote, with different bytes.
    pub differences: Vec<FileDifference>,
}

impl OutputComparison {
    /// Whether krusty wrote exactly kotlinc's files.
    pub fn is_identical(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty() && self.differences.is_empty()
    }

    pub fn missing_classes(&self) -> usize {
        self.missing.iter().filter(|path| is_class(path)).count()
    }

    pub fn extra_classes(&self) -> usize {
        self.extra.iter().filter(|path| is_class(path)).count()
    }
}

/// One file both compilers wrote differently, and why: a class's primary
/// [`DifferenceKind`](krusty::jvm::class_comparison::DifferenceKind) identifier, `kotlin-module`
/// for the module's `.kotlin_module` file, or `other-file`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDifference {
    pub path: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

fn is_class(path: &str) -> bool {
    path.ends_with(".class")
}

/// Compare every file under `candidate` (krusty's output) with the same file under `reference`
/// (kotlinc's).
pub fn compare_outputs(reference: &Path, candidate: &Path) -> std::io::Result<OutputComparison> {
    let reference_files = files(reference)?;
    let candidate_files = files(candidate)?;
    let mut comparison = OutputComparison {
        classes: reference_files.keys().filter(|path| is_class(path)).count(),
        ..OutputComparison::default()
    };
    for (path, reference_file) in &reference_files {
        let Some(candidate_file) = candidate_files.get(path) else {
            comparison.missing.push(path.clone());
            continue;
        };
        let (left, right) = (
            std::fs::read(reference_file)?,
            std::fs::read(candidate_file)?,
        );
        if left == right {
            if is_class(path) {
                comparison.identical_classes += 1;
            }
            continue;
        }
        let (kind, member) = if is_class(path) {
            let difference =
                compare_classes(&left, &right).expect("different bytes are a difference");
            (difference.kind.id().to_string(), difference.member)
        } else if path.ends_with(".kotlin_module") {
            ("kotlin-module".to_string(), None)
        } else {
            ("other-file".to_string(), None)
        };
        comparison.differences.push(FileDifference {
            path: path.clone(),
            kind,
            member,
        });
    }
    comparison.extra = candidate_files
        .keys()
        .filter(|path| !reference_files.contains_key(*path))
        .cloned()
        .collect();
    Ok(comparison)
}

/// Every file under `root`, by its `/`-separated relative path. A directory that does not exist
/// holds no file: a compilation without sources writes none.
fn files(root: &Path) -> std::io::Result<BTreeMap<String, PathBuf>> {
    let mut found = BTreeMap::new();
    if !root.exists() {
        return Ok(found);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("a walked file is under its root")
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                found.insert(relative, path);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, bytes: &[u8]) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "krusty-verify-outputs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn files_only_one_compiler_wrote_are_missing_or_extra() {
        let root = scratch("sets");
        let (kotlinc, krusty) = (root.join("kotlinc"), root.join("krusty"));
        write(&kotlinc, "a/Same.class", b"same");
        write(&krusty, "a/Same.class", b"same");
        write(&kotlinc, "a/Gone.class", b"gone");
        write(&krusty, "a/New.class", b"new");
        write(&kotlinc, "META-INF/main.kotlin_module", b"one");
        write(&krusty, "META-INF/main.kotlin_module", b"two");
        let comparison = compare_outputs(&kotlinc, &krusty).unwrap();
        assert_eq!(
            comparison,
            OutputComparison {
                classes: 2,
                identical_classes: 1,
                missing: vec!["a/Gone.class".to_string()],
                extra: vec!["a/New.class".to_string()],
                differences: vec![FileDifference {
                    path: "META-INF/main.kotlin_module".to_string(),
                    kind: "kotlin-module".to_string(),
                    member: None,
                }],
            }
        );
        assert!(!comparison.is_identical());
        assert_eq!(
            (comparison.missing_classes(), comparison.extra_classes()),
            (1, 1)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_differing_class_gets_its_primary_kind() {
        let root = scratch("kinds");
        let (kotlinc, krusty) = (root.join("kotlinc"), root.join("krusty"));
        write(&kotlinc, "A.class", b"not a class");
        write(&krusty, "A.class", b"not one either");
        assert_eq!(
            compare_outputs(&kotlinc, &krusty).unwrap().differences,
            vec![FileDifference {
                path: "A.class".to_string(),
                kind: "unreadable".to_string(),
                member: None,
            }]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absent_output_directories_hold_no_files() {
        let root = scratch("absent");
        assert_eq!(
            compare_outputs(&root.join("kotlinc"), &root.join("krusty")).unwrap(),
            OutputComparison::default()
        );
    }
}
