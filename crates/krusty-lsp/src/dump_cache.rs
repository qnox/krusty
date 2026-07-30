//! On-disk store for dev-mode dumps.
//!
//! Unlike `deps_cache`, this is keyed by source path rather than content, so a dump keeps the same
//! path across runs and an editor buffer left open on it refreshes in place. Files are written as
//! `.md` so the language server does not treat its own dumps as Kotlin sources.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `text` as the dump for `key` and return its stable path.
pub fn store(root: &Path, key: &str, text: &str) -> io::Result<PathBuf> {
    let path = dump_path(root, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a reloading editor never sees a half-written dump. The temp file gets a
    // per-invocation unique name so concurrent stores of the same key never share (and thus never
    // interleave through) the same temp file; it lives beside the final path so the rename stays
    // on one filesystem and therefore stays atomic.
    let tmp = tmp_path(&path);
    fs::write(&tmp, text)?;
    if let Err(err) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(path)
}

/// A unique temp path in the same directory as `path`, for one `store` invocation.
fn tmp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dump");
    path.with_file_name(format!("{file_name}.{}-{count}.tmp", std::process::id()))
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

    #[test]
    fn temp_paths_are_unique_per_invocation() {
        let root = scratch("tmp-unique");
        let path = dump_path(&root, "src/Main.kt");

        let first = tmp_path(&path);
        let second = tmp_path(&path);

        assert_ne!(
            first, second,
            "concurrent stores must not share a temp file"
        );
        assert_eq!(
            first.parent(),
            path.parent(),
            "temp file must stay beside the final path so rename is atomic"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_stores_of_the_same_key_never_interleave_or_leave_temp_files() {
        let root = scratch("concurrent");
        let text_a = "a".repeat(200_000);
        let text_b = "b".repeat(200_000);

        let root_a = root.clone();
        let text_a_thread = text_a.clone();
        let handle_a = std::thread::spawn(move || store(&root_a, "src/Main.kt", &text_a_thread));

        let root_b = root.clone();
        let text_b_thread = text_b.clone();
        let handle_b = std::thread::spawn(move || store(&root_b, "src/Main.kt", &text_b_thread));

        let path_a = handle_a.join().unwrap().unwrap();
        let path_b = handle_b.join().unwrap().unwrap();
        assert_eq!(path_a, path_b, "the key's path must stay stable");

        let final_text = fs::read_to_string(&path_a).unwrap();
        assert!(
            final_text == text_a || final_text == text_b,
            "final dump must hold one writer's complete text, never interleaved bytes"
        );

        let dumps_dir = path_a.parent().unwrap();
        for entry in fs::read_dir(dumps_dir).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.ends_with(".tmp"),
                "stray temp file left behind: {name}"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }
}
