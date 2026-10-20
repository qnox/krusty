//! Where a module's main fragment keeps its sources and resources (`layout:`), and the source
//! files found there.

use std::io;
use std::path::{Path, PathBuf};

/// The directories of a module's main fragment.
pub(super) struct Layout {
    /// The source directories, Kotlin and Java alike.
    pub sources: Vec<PathBuf>,
    pub resources: PathBuf,
}

/// The main fragment's directories in the module directory `module` for the `layout:` value:
/// `src` and `resources` by default, Maven's `src/main/kotlin`, `src/main/java` and
/// `src/main/resources` for `maven-like`.
pub(super) fn layout(module: &Path, layout: &str) -> Layout {
    if layout == "maven-like" {
        let main = module.join("src").join("main");
        Layout {
            sources: vec![main.join("kotlin"), main.join("java")],
            resources: main.join("resources"),
        }
    } else {
        Layout {
            sources: vec![module.join("src")],
            resources: module.join("resources"),
        }
    }
}

/// The source files below a fragment's source directories, each kind sorted.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Sources {
    pub kotlin: Vec<PathBuf>,
    pub java: Vec<PathBuf>,
}

/// The `.kt` and `.java` files below `directories`; a directory that does not exist holds none.
/// Symbolic links are not followed.
pub(super) fn find(directories: &[PathBuf]) -> io::Result<Sources> {
    let mut sources = Sources::default();
    for directory in directories {
        if directory.is_dir() {
            walk(directory, &mut sources)?;
        }
    }
    sources.kotlin.sort();
    sources.java.sort();
    Ok(sources)
}

fn walk(directory: &Path, sources: &mut Sources) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&path, sources)?;
        } else if kind.is_file() {
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("kt") => sources.kotlin.push(path),
                Some("java") => sources.java.push(path),
                _ => {}
            }
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
        let kotlin = &layout.sources[0];
        std::fs::create_dir_all(kotlin.join("b/c")).unwrap();
        for file in ["b/c/Z.kt", "A.kt", "b/notes.md", "b/Script.kts"] {
            std::fs::write(kotlin.join(file), "").unwrap();
        }
        std::fs::create_dir_all(&layout.sources[1]).unwrap();
        std::fs::write(layout.sources[1].join("J.java"), "").unwrap();
        assert_eq!(
            find(&layout.sources).unwrap(),
            Sources {
                kotlin: vec![kotlin.join("A.kt"), kotlin.join("b/c/Z.kt")],
                java: vec![layout.sources[1].join("J.java")],
            }
        );
        assert_eq!(layout.resources, root.join("src/main/resources"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
