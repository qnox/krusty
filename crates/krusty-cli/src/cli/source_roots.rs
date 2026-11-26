//! The source files a command line names, collected as kotlinc collects its source roots: a root
//! repeated with the same spelling is reported and read once, and a file reached through two roots
//! (a directory and a file inside it, or two lexical spellings of one path) is compiled once, silently.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::CliWarning;

/// The roots and files collected so far.
#[derive(Default)]
pub(super) struct SourceRoots {
    roots: HashSet<String>,
    files: HashSet<PathBuf>,
}

impl SourceRoots {
    /// Append the files under `root` that no earlier root reached, and return them. `None` when
    /// `root` repeats an earlier root's spelling, which kotlinc reports with
    /// [`duplicate_source_root`] and skips.
    pub(super) fn collect(&mut self, root: &str, sources: &mut Vec<String>) -> Option<Vec<String>> {
        if !self.roots.insert(root.to_string()) {
            return None;
        }
        let mut found = Vec::new();
        walk(&folded(root).to_string_lossy(), &mut found);
        let collected: Vec<String> = found
            .into_iter()
            .filter(|file| self.files.insert(identity(file)))
            .collect();
        sources.extend(collected.iter().cloned());
        Some(collected)
    }
}

/// kotlinc's warning for a source root named twice.
pub(super) fn duplicate_source_root(root: &str) -> CliWarning {
    CliWarning {
        name: Some(krusty::diag::names::ROOTS_RESOLUTION_WARNING),
        leveled: true,
        message: format!("Duplicate source root: {root}"),
    }
}

/// The path a spelling names, as kotlinc reads it: `.` and `..` folded lexically, never through a
/// symlink, and still relative when it was written relative.
fn folded(file: &str) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in Path::new(file).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match folded.components().next_back() {
                Some(Component::Normal(_)) => {
                    folded.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => folded.push(Component::ParentDir),
            },
            other => folded.push(other),
        }
    }
    if folded.as_os_str().is_empty() {
        folded.push(Component::CurDir);
    }
    folded
}

/// The file a spelling names, as kotlinc compares source files: the absolute path with `.` and
/// `..` folded lexically, so one file has one identity however it is spelled.
fn identity(file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        return folded(file);
    }
    let absolute =
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path));
    folded(&absolute.to_string_lossy())
}

fn walk(path: &str, out: &mut Vec<String>) {
    let p = Path::new(path);
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                walk(&e.to_string_lossy(), out);
            }
        }
    } else if krusty::source::is_batch_compilable_path(p) {
        out.push(path.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krusty_roots_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/G.kt"), "fun g() = 1\n").unwrap();
        std::fs::write(dir.join("src/H.kt"), "fun h() = 2\n").unwrap();
        dir
    }

    fn collect(roots: &[String]) -> (Vec<String>, Vec<Option<Vec<String>>>) {
        let mut source_roots = SourceRoots::default();
        let mut sources = Vec::new();
        let collected = roots
            .iter()
            .map(|root| source_roots.collect(root, &mut sources))
            .collect();
        (sources, collected)
    }

    #[test]
    fn a_repeated_spelling_is_skipped_for_its_warning() {
        let dir = layout("repeated");
        let g = dir.join("src/G.kt").display().to_string();
        assert_eq!(
            collect(&[g.clone(), g.clone()]),
            (vec![g.clone()], vec![Some(vec![g]), None])
        );
    }

    #[test]
    fn a_file_reached_twice_is_collected_once_in_its_first_spelling() {
        let dir = layout("reached");
        let src = dir.join("src").display().to_string();
        let g = dir.join("src/G.kt").display().to_string();
        let h = dir.join("src/H.kt").display().to_string();
        let dotted = dir.join("src/./G.kt").display().to_string();
        assert_eq!(
            collect(&[g.clone(), src, dotted]),
            (
                vec![g.clone(), h.clone()],
                vec![Some(vec![g]), Some(vec![h]), Some(Vec::new())]
            )
        );
    }

    #[test]
    fn a_symlinked_spelling_stays_a_distinct_input() {
        let dir = layout("symlink");
        std::os::unix::fs::symlink(dir.join("src"), dir.join("alias")).unwrap();
        let g = dir.join("src/G.kt").display().to_string();
        let aliased = dir.join("alias/G.kt").display().to_string();
        assert_eq!(
            collect(&[g.clone(), aliased.clone()]),
            (
                vec![g.clone(), aliased.clone()],
                vec![Some(vec![g]), Some(vec![aliased])]
            )
        );
    }

    #[test]
    fn a_path_that_folds_away_names_the_current_directory() {
        assert_eq!(
            ["", ".", "./.", "src/..", "./src/../."].map(folded),
            [
                PathBuf::from("."),
                PathBuf::from("."),
                PathBuf::from("."),
                PathBuf::from("."),
                PathBuf::from(".")
            ]
        );
        assert_eq!(identity("."), std::env::current_dir().unwrap());
    }

    #[test]
    fn a_leading_parent_is_resolved_against_the_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        let name = cwd.file_name().unwrap().to_string_lossy().into_owned();
        let up_and_down = format!("../{name}/Cargo.toml");
        assert_eq!(identity(&up_and_down), cwd.join("Cargo.toml"));
        assert_eq!(folded("../x/./y/.."), PathBuf::from("../x"));
    }

    #[test]
    fn the_warning_is_kotlincs_roots_resolution_warning() {
        assert_eq!(
            duplicate_source_root("src/G.kt"),
            CliWarning {
                name: Some(krusty::diag::names::ROOTS_RESOLUTION_WARNING),
                leveled: true,
                message: "Duplicate source root: src/G.kt".to_string(),
            }
        );
    }
}
