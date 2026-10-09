//! Differential cases: a project's files, given to JetBrains' `kotlin` (the oracle, [`oracle`]) and
//! to krusty-toolchain, whose reports must agree.
//!
//! A case file holds the files after `--- <path>` lines. Lines before the first section describe
//! the case. A `--- krusty` section holds what krusty-toolchain reports instead where it
//! deliberately differs; the toolchain's own output is never written into the repository.

pub mod kotlin;
pub mod oracle;

use std::path::{Path, PathBuf};

/// The section that holds krusty-toolchain's deliberate difference rather than a file.
const KRUSTY: &str = "krusty";

pub struct Case {
    pub name: String,
    /// The project's files, by `/`-separated path below its root.
    pub files: Vec<(String, String)>,
    /// What krusty-toolchain reports instead, where it deliberately refuses what the toolchain
    /// accepts, or rejects for another reason.
    pub krusty: Option<Vec<String>>,
}

/// The cases in `tests/cases/<directory>`, by file name.
pub fn cases(directory: &str) -> Vec<Case> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cases")
        .join(directory);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "case")
        })
        .collect();
    paths.sort();
    paths.iter().map(|path| read_case(path)).collect()
}

fn read_case(path: &Path) -> Case {
    let text = std::fs::read_to_string(path).unwrap();
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix("--- ") {
            Some(name) => sections.push((name.to_string(), Vec::new())),
            None => {
                if let Some((_, lines)) = sections.last_mut() {
                    lines.push(line);
                }
            }
        }
    }
    let mut case = Case {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        files: Vec::new(),
        krusty: None,
    };
    for (name, lines) in sections {
        if name == KRUSTY {
            case.krusty = Some(lines.iter().map(|line| line.to_string()).collect());
            continue;
        }
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        case.files.push((name, text));
    }
    case
}

/// A directory removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    /// A fresh, empty directory named after `name` and this process.
    pub fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("krusty-toolchain-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write `files` below a directory named `project` in `temp`, the name the oracle's project has
/// too, since the root module is named after its directory. Returns the project root.
pub fn materialize(temp: &TempDir, files: &[(String, String)]) -> PathBuf {
    let root = temp.0.join("project");
    std::fs::create_dir_all(&root).unwrap();
    for (file, text) in files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}
