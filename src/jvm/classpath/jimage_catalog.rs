//! JDK jimage class locations and their shared package catalog.
//!
//! The location index and package catalog retain one `NameTree`, so a class and its package keep
//! the same `NameId` in both views. The completed index owns that tree through an `Arc`; no source
//! spelling is promoted to process-lifetime storage independently of the cached index.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::name_tree::{NameId, NameTree};

use super::{entry_stamp, EntryKey, JarPackages};

/// One jimage resource: `(file offset, on-disk byte size, zlib-compressed?)`.
pub(super) type JimageEntry = (u64, usize, bool);

#[derive(Debug, Default)]
pub(super) struct JimageIndex {
    names: Arc<NameTree>,
    by_name: HashMap<NameId, JimageEntry>,
}

impl JimageIndex {
    pub(super) fn len(&self) -> usize {
        self.by_name.len()
    }

    pub(super) fn entry(&self, internal: &str) -> Option<JimageEntry> {
        let id = self.names.get(internal)?;
        self.by_name.get(&id).copied()
    }

    /// Project the completed location index into the package view without copying its name tree.
    pub(super) fn package_catalog(&self) -> JarPackages {
        let mut catalog = JarPackages::with_names(Arc::clone(&self.names));
        for &class in self.by_name.keys() {
            let Some(package) = self.names.parent(class) else {
                continue;
            };
            catalog.classes.push(class);
            if package != NameTree::ROOT {
                catalog.packages.entry(package).or_default().has_classes = true;
            }
        }
        catalog.complete = !self.by_name.is_empty();
        catalog
    }
}

/// Process-global jimage indexes, keyed by the exact entry snapshot. A JDK image is immutable for
/// the lifetime represented by its `EntryKey`; parsing its location table once avoids a 146 MB scan
/// for every compiler worker while the index and its shared name tree remain under one owner.
fn global_cache() -> &'static Mutex<HashMap<EntryKey, Arc<JimageIndex>>> {
    static CACHE: OnceLock<Mutex<HashMap<EntryKey, Arc<JimageIndex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn cached_jimage_index(path: &Path) -> Option<Arc<JimageIndex>> {
    let key = EntryKey {
        path: path.to_path_buf(),
        stamp: entry_stamp(path),
        jdk_release: None,
    };
    let mut cache = global_cache().lock().unwrap();
    if let Some(index) = cache.get(&key) {
        return Some(Arc::clone(index));
    }
    let index = Arc::new(build_index(path)?);
    cache.insert(key, Arc::clone(&index));
    Some(index)
}

/// Class identity `parent/base` assembled without allocating or re-parsing the joined spelling.
fn class_id(names: &NameTree, parent: &str, base: &str) -> NameId {
    let mut id = NameTree::ROOT;
    for segment in parent.split('/').filter(|segment| !segment.is_empty()) {
        id = names.child_of(id, segment);
    }
    names.child_of(id, base)
}

/// Build the jimage class-location index directly from its location table.
fn build_index(path: &Path) -> Option<JimageIndex> {
    // Read only the header and location/string tables. Class bytes are seek-read on demand.
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; 28];
    file.read_exact(&mut head).ok()?;
    let header_word = |offset: usize| {
        u32::from_le_bytes([
            head[offset],
            head[offset + 1],
            head[offset + 2],
            head[offset + 3],
        ]) as usize
    };
    if header_word(0) != 0xCAFE_DADA {
        return None;
    }
    let table_length = header_word(16);
    let locations_size = header_word(20);
    let strings_size = header_word(24);
    let header = 28;
    let offsets = header + table_length * 4;
    let locations = offsets + table_length * 4;
    let strings = locations + locations_size;
    let content = strings + strings_size;
    let mut bytes = vec![0u8; content];
    file.rewind().ok()?;
    file.read_exact(&mut bytes).ok()?;

    let table_word = |offset: usize| {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("validated table word"),
        )
    };
    let read_string = |offset: usize| -> &str {
        if offset == 0 {
            return "";
        }
        let start = strings + offset;
        let mut end = start;
        while end < bytes.len() && bytes[end] != 0 {
            end += 1;
        }
        std::str::from_utf8(&bytes[start..end]).unwrap_or("")
    };
    // ImageLocation attributes: 2=PARENT, 3=BASE, 4=EXTENSION, 5=OFFSET,
    // 6=COMPRESSED, 7=UNCOMPRESSED.
    let decode = |mut cursor: usize| -> [usize; 8] {
        let mut attributes = [0usize; 8];
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            cursor += 1;
            let kind = (byte >> 3) as usize;
            if kind == 0 {
                break;
            }
            let length = ((byte & 0x7) + 1) as usize;
            let mut value = 0usize;
            for _ in 0..length {
                if cursor >= bytes.len() {
                    break;
                }
                value = (value << 8) | bytes[cursor] as usize;
                cursor += 1;
            }
            if kind < attributes.len() {
                attributes[kind] = value;
            }
        }
        attributes
    };

    let mut index = JimageIndex::default();
    for slot in 0..table_length {
        let location_offset = table_word(offsets + slot * 4) as usize;
        if location_offset == 0 {
            continue;
        }
        let attributes = decode(locations + location_offset);
        if read_string(attributes[4]) != "class" {
            continue;
        }
        let parent = read_string(attributes[2]);
        let base = read_string(attributes[3]);
        if parent.is_empty() || base.is_empty() {
            continue;
        }
        let (offset, compressed, uncompressed) = (attributes[5], attributes[6], attributes[7]);
        let stored = if compressed != 0 {
            compressed
        } else {
            uncompressed
        };
        let class = class_id(&index.names, parent, base);
        index
            .by_name
            .entry(class)
            .or_insert(((content + offset) as u64, stored, compressed != 0));
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_catalog_share_exact_class_and_package_identities() {
        let mut index = JimageIndex::default();
        let class = class_id(&index.names, "sample/catalog", "Widget");
        index.by_name.insert(class, (1, 2, false));

        assert_eq!(index.entry("sample/catalog/Widget"), Some((1, 2, false)));
        let package = index.names.parent(class).expect("class package");
        let catalog = index.package_catalog();

        assert!(Arc::ptr_eq(&catalog.names, &index.names));
        assert_eq!(catalog.classes, vec![class]);
        assert!(catalog.packages[&package].has_classes);
        assert_eq!(catalog.names.get("sample/catalog/Widget"), Some(class));
        assert_eq!(catalog.names.get("sample/catalog"), Some(package));
    }
}
