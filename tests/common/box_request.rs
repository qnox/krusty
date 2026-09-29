//! Class bytes for one `box()` request.
//!
//! Directory classpath entries are read once per path and then borrowed. A later `box()` against
//! the same directory does not copy those class files before the runner frame is built.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub(super) type ClassSet = Vec<(String, Vec<u8>)>;

/// Recursively collect `(internal_name, bytes)` for every `.class` under a directory classpath
/// entry, memoized by directory path. Safe to memoize: cached lib dirs are immutable once published
/// (`compile_libs`), and per-test scratch dirs are unique per allocation, so a path's contents never
/// change between calls within one process.
pub(super) fn dir_classes(dir: &Path) -> Option<Arc<ClassSet>> {
    type DirClassesMemo = Mutex<HashMap<PathBuf, Arc<ClassSet>>>;
    static MEMO: OnceLock<DirClassesMemo> = OnceLock::new();
    let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = memo
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dir)
        .cloned()
    {
        return Some(hit);
    }
    fn walk(root: &Path, dir: &Path, out: &mut ClassSet) -> Option<()> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let rel = path.strip_prefix(root).ok()?;
                let name = rel
                    .to_string_lossy()
                    .trim_end_matches(".class")
                    .replace(std::path::MAIN_SEPARATOR, "/");
                out.push((name, std::fs::read(&path).ok()?));
            }
        }
        Some(())
    }
    let mut classes = ClassSet::new();
    walk(dir, dir, &mut classes)?;
    let arc = Arc::new(classes);
    memo.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dir.to_path_buf(), Arc::clone(&arc));
    Some(arc)
}

/// Append borrowed class bytes from memoized directory sets. `parts` aliases `held`.
pub(super) fn borrow_dir_classes<'a>(
    held: &'a [Arc<ClassSet>],
    parts: &mut Vec<(&'a str, &'a [u8])>,
) {
    for set in held {
        for (name, bytes) in set.iter() {
            parts.push((name.as_str(), bytes.as_slice()));
        }
    }
}

/// The BoxRunner request frame: `[u64 id][u32 n][for each: u16 name_len, name, u32 data_len, data][u16 box_len, box_name]`.
pub(super) fn frame_box_request(id: u64, classes: &[(&str, &[u8])], box_class: &str) -> Vec<u8> {
    let mut size = 8 + 4 + 2 + box_class.len();
    for (name, data) in classes {
        size += 2 + name.len() + 4 + data.len();
    }
    let mut buf = Vec::with_capacity(size);
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&(classes.len() as u32).to_be_bytes());
    for (name, data) in classes {
        buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
        buf.extend_from_slice(data);
    }
    buf.extend_from_slice(&(box_class.len() as u16).to_be_bytes());
    buf.extend_from_slice(box_class.as_bytes());
    debug_assert_eq!(buf.len(), size);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take<'a>(buf: &mut &'a [u8], n: usize) -> &'a [u8] {
        let (head, tail) = buf.split_at(n);
        *buf = tail;
        head
    }

    #[test]
    fn directory_classes_are_read_once_and_borrowed() {
        let dir = super::super::scratch_dir()
            .expect("scratch")
            .join("classes");
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        let bytes = b"class-bytes-6044";
        std::fs::write(dir.join("pkg").join("A.class"), bytes).unwrap();
        let first = dir_classes(&dir).unwrap();
        let second = dir_classes(&dir).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let held = [Arc::clone(&first)];
        let mut parts = Vec::new();
        borrow_dir_classes(&held, &mut parts);
        assert_eq!(parts, [("pkg/A", bytes.as_slice())]);
        assert!(std::ptr::eq(parts[0].1, first[0].1.as_slice()));
    }

    #[test]
    fn box_request_frame_matches_the_runner_layout() {
        let class = b"\xca\xfe";
        let framed = frame_box_request(0x11, &[("pkg/A", class)], "pkg.A");
        assert_eq!(framed.len(), 8 + 4 + 2 + 5 + 4 + class.len() + 2 + 5);
        let mut rest = framed.as_slice();
        let id = u64::from_be_bytes(take(&mut rest, 8).try_into().unwrap());
        assert_eq!(id, 0x11);
        let count = u32::from_be_bytes(take(&mut rest, 4).try_into().unwrap());
        assert_eq!(count, 1);
        let name_len = u16::from_be_bytes(take(&mut rest, 2).try_into().unwrap()) as usize;
        assert_eq!(take(&mut rest, name_len), b"pkg/A");
        let data_len = u32::from_be_bytes(take(&mut rest, 4).try_into().unwrap()) as usize;
        assert_eq!(take(&mut rest, data_len), class);
        let box_len = u16::from_be_bytes(take(&mut rest, 2).try_into().unwrap()) as usize;
        assert_eq!(take(&mut rest, box_len), b"pkg.A");
        assert!(rest.is_empty());
    }
}
