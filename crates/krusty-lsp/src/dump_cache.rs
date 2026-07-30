//! On-disk store for dev-mode dumps.
//!
//! Unlike `deps_cache`, this is keyed by source path rather than content, so a dump keeps the same
//! path across runs and an editor buffer left open on it refreshes in place. Files are written as
//! `.md` so the language server does not treat its own dumps as Kotlin sources.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Write `text` as the dump for `key` and return its stable path.
pub fn store(root: &Path, key: &str, text: &str) -> io::Result<PathBuf> {
    let path = dump_path(root, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a reloading editor never sees a half-written dump.
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// `<root>/dumps/<escaped key>.krusty.md`.
pub fn dump_path(root: &Path, key: &str) -> PathBuf {
    let mut path = root.join("dumps");
    let segments: Vec<&str> = key.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let (last, leading) = match segments.split_last() {
        Some((last, leading)) => (*last, leading),
        None => ("dump", &[][..]),
    };
    for segment in leading {
        path.push(escape(segment));
    }
    path.push(format!("{}.krusty.md", escape(last)));
    path
}

/// Percent-ish escaping that keeps a key inside the root and off reserved names.
fn escape(segment: &str) -> String {
    if segment == "." || segment == ".." {
        return segment
            .bytes()
            .map(|byte| format!("_{byte:02x}"))
            .collect::<String>();
    }
    segment
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch.to_string()
            } else {
                format!("_{:02x}", ch as u32)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krusty-dump-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_path_is_stable_across_content_changes() {
        let root = scratch("stable");
        let first = store(&root, "src/main/kotlin/Main.kt", "one").unwrap();
        let second = store(&root, "src/main/kotlin/Main.kt", "two").unwrap();

        assert_eq!(
            first, second,
            "the dump path must not move when content changes"
        );
        assert_eq!(fs::read_to_string(&second).unwrap(), "two");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dumps_are_markdown_under_the_dumps_subtree() {
        let root = scratch("layout");
        let path = store(&root, "src/Main.kt", "body").unwrap();

        assert!(path.starts_with(root.join("dumps")), "{path:?}");
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("md"));
        assert!(
            path.to_string_lossy().ends_with("Main.kt.krusty.md"),
            "{path:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keys_cannot_escape_the_root() {
        let root = scratch("escape");
        let path = store(&root, "../../etc/passwd", "body").unwrap();

        assert!(path.starts_with(&root), "{path:?}");
        assert!(!path.to_string_lossy().contains(".."), "{path:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn distinct_sources_get_distinct_paths() {
        let root = scratch("distinct");
        let first = store(&root, "a/Main.kt", "x").unwrap();
        let second = store(&root, "b/Main.kt", "x").unwrap();

        assert_ne!(first, second);
        let _ = fs::remove_dir_all(&root);
    }
}
