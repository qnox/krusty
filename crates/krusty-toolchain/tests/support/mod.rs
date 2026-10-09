//! Recorded cases: a project's files and what JetBrains' `kotlin` printed for it, each after a
//! `--- <name>` line (see `scripts/kotlin-toolchain/record_projects.py`).

use std::path::{Path, PathBuf};

use krusty_toolchain::diagnostic::Diagnostics;

pub struct Case {
    pub name: String,
    /// The project's files, by path below its root.
    pub files: Vec<(String, String)>,
    /// The recorded sections, by name.
    pub recorded: Vec<(String, Vec<String>)>,
}

impl Case {
    pub fn section(&self, name: &str) -> Option<&Vec<String>> {
        self.recorded
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, lines)| lines)
    }
}

/// The cases in `tests/recorded/<directory>`, by file name. `recorded` names the sections that
/// are recorded output rather than files.
pub fn cases(directory: &str, recorded: &[&str]) -> Vec<Case> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/recorded")
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
    paths.iter().map(|path| read_case(path, recorded)).collect()
}

fn read_case(path: &Path, recorded: &[&str]) -> Case {
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
        recorded: Vec::new(),
    };
    for (name, lines) in sections {
        if recorded.contains(&name.as_str()) {
            let owned = lines.iter().map(|line| line.to_string()).collect();
            case.recorded.push((name, owned));
        } else {
            let mut text = lines.join("\n");
            if !lines.is_empty() {
                text.push('\n');
            }
            case.files.push((name, text));
        }
    }
    case
}

pub struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sections below this directory are artifacts of the case's local Maven repository.
pub const REPOSITORY_SECTION: &str = "m2/";

/// Write the case's files below a fresh directory named `project`, as the recorder does (the root
/// module is named after it); the directory is removed when the [`TempDir`] is dropped. The case's
/// repository artifacts are not part of the project.
pub fn materialize(kind: &str, case: &Case) -> (TempDir, PathBuf) {
    let temp = std::env::temp_dir().join(format!(
        "krusty-toolchain-{kind}-case-{}-{}",
        case.name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&temp);
    let root = temp.join("project");
    for (file, text) in &case.files {
        if file.starts_with(REPOSITORY_SECTION) {
            continue;
        }
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let root = root.canonicalize().unwrap();
    (TempDir(temp), root)
}

/// The problems as the recorder writes them: paths relative to `root`, line breaks as `\n`.
pub fn reported(root: &Path, diagnostics: &Diagnostics) -> Vec<String> {
    let prefix = format!("{}/", root.display());
    diagnostics
        .iter()
        .map(|diagnostic| {
            diagnostic
                .to_string()
                .replace(&prefix, "")
                .replace('\n', "\\n")
        })
        .collect()
}
