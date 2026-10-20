//! Where a module's main fragment keeps its sources and resources (`layout:`), and the source
//! files found there.

use std::io;
use std::path::{Path, PathBuf};

/// The directories of a module's main fragment.
pub(super) struct Layout {
    /// The source directories, Kotlin and Java alike, in the toolchain's order.
    pub sources: Vec<PathBuf>,
    pub resources: PathBuf,
}

/// The main fragment's directories in the module directory `module` for the `layout:` value:
/// `src` and `resources` by default, Maven's `src/main/java`, `src/main/kotlin` and
/// `src/main/resources` for `maven-like`.
pub(super) fn layout(module: &Path, layout: &str) -> Layout {
    if layout == "maven-like" {
        let main = module.join("src").join("main");
        Layout {
            sources: vec![main.join("java"), main.join("kotlin")],
            resources: main.join("resources"),
        }
    } else {
        Layout {
            sources: vec![module.join("src")],
            resources: module.join("resources"),
        }
    }
}

/// The source files below a fragment's source directories.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Sources {
    /// The `.kt` files, sorted.
    pub kotlin: Vec<PathBuf>,
    /// The `.java` files, sorted.
    pub java: Vec<PathBuf>,
    /// The `.kt` and `.java` files in the order the toolchain walks the directories: each
    /// directory in turn, depth first, its entries in the order the file system lists them.
    pub walked: Vec<PathBuf>,
    /// How many of the directories are directories holding anything at all, which is how many
    /// source directories the toolchain compiles.
    pub filled: usize,
    /// The directories that exist without being directories, which the toolchain refuses.
    pub not_directories: Vec<PathBuf>,
}

/// The `.kt` and `.java` files below `directories`; a directory that does not exist holds none.
/// Symbolic links below a directory are not followed.
pub(super) fn find(directories: &[PathBuf]) -> io::Result<Sources> {
    let mut sources = Sources::default();
    for directory in directories {
        if directory.is_dir() {
            if std::fs::read_dir(directory)?.next().is_some() {
                sources.filled += 1;
            }
            walk(directory, &mut sources.walked)?;
        } else if std::fs::symlink_metadata(directory).is_ok() {
            sources.not_directories.push(directory.clone());
        }
    }
    for file in &sources.walked {
        match file.extension().and_then(|extension| extension.to_str()) {
            Some("kt") => sources.kotlin.push(file.clone()),
            _ => sources.java.push(file.clone()),
        }
    }
    sources.kotlin.sort();
    sources.java.sort();
    Ok(sources)
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&path, files)?;
        } else if kind.is_file()
            && matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("kt" | "java")
            )
        {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kotlin_and_java_files_are_sources_and_each_kind_is_sorted() {
        let root = std::env::temp_dir().join(format!("krusty-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = layout(&root, "maven-like");
        let [java, kotlin] = [&layout.sources[0], &layout.sources[1]];
        std::fs::create_dir_all(kotlin.join("b/c")).unwrap();
        for file in ["b/c/Z.kt", "A.kt", "b/notes.md", "b/Script.kts"] {
            std::fs::write(kotlin.join(file), "").unwrap();
        }
        std::fs::create_dir_all(java).unwrap();
        std::fs::write(java.join("J.java"), "").unwrap();
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let file = root.join("file");
        std::fs::write(&file, "").unwrap();
        let mut directories = layout.sources.clone();
        directories.extend([empty, file.clone(), root.join("missing")]);
        let found = find(&directories).unwrap();
        // Within a directory the file system's order is kept, so only the directories' order is
        // asserted here.
        let mut walked_after_java = found.walked[1..].to_vec();
        walked_after_java.sort();
        assert_eq!(
            (found.walked[0].clone(), walked_after_java),
            (
                java.join("J.java"),
                vec![kotlin.join("A.kt"), kotlin.join("b/c/Z.kt")]
            )
        );
        assert_eq!(
            Sources {
                walked: Vec::new(),
                ..found
            },
            Sources {
                kotlin: vec![kotlin.join("A.kt"), kotlin.join("b/c/Z.kt")],
                java: vec![java.join("J.java")],
                walked: Vec::new(),
                filled: 2,
                not_directories: vec![file],
            }
        );
        assert_eq!(layout.resources, root.join("src/main/resources"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_holding_only_other_files_is_filled() {
        let root = std::env::temp_dir().join(format!("krusty-filled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("hidden")).unwrap();
        std::fs::write(root.join("hidden/.marker"), "").unwrap();
        let found = find(&[root.join("hidden")]).unwrap();
        assert_eq!(
            found,
            Sources {
                filled: 1,
                ..Sources::default()
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
