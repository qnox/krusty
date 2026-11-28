//! JDK jimage class locations and their shared package catalog.
//!
//! The location index and package catalog retain one `NameTree`, so a class and its package keep
//! the same `NameId` in both views. The completed index owns that tree through an `Arc`; no source
//! spelling is promoted to process-lifetime storage independently of the cached index.

use crate::fx_hash::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::name_tree::{NameId, NameTree};

use super::jimage_locations::JimageLocations;
use super::jimage_modules::JimageModule;
use super::{entry_stamp, EntryKey, JarPackages};

#[derive(Debug)]
pub(super) struct JimageIndex {
    names: Arc<NameTree>,
    locations: JimageLocations,
    path: std::path::PathBuf,
    modules: Vec<JimageModule>,
    /// The module whose image resources hold each package's classes. A named module owns its
    /// packages exclusively, so one package maps to one module.
    package_modules: HashMap<NameId, u32>,
}

impl Default for JimageIndex {
    fn default() -> Self {
        Self {
            names: Arc::new(NameTree::default()),
            locations: JimageLocations::default(),
            path: std::path::PathBuf::new(),
            modules: Vec::new(),
            package_modules: HashMap::default(),
        }
    }
}

impl JimageIndex {
    pub(super) fn len(&self) -> usize {
        self.locations.class_count()
    }

    pub(super) fn entry(&self, internal: &str) -> Option<(u64, usize, bool)> {
        let id = self.names.get(internal)?;
        self.locations.get(id)
    }

    /// The module declaring `class` when its package is unreachable from code outside every named
    /// module, because the module's descriptor has no unqualified `exports` for it. `None` for a
    /// class the image does not hold, or whose package is exported.
    pub(super) fn unexported_class_module(&self, class: &str) -> Option<&str> {
        let package = self.names.parent(self.names.get(class)?)?;
        let module = &self.modules[*self.package_modules.get(&package)? as usize];
        let exports = module.exports(&self.names, |location| self.read_entry(location))?;
        (!exports.contains(&package)).then_some(module.name())
    }

    /// Read one resource's bytes. A "zip"-compressed resource (the JetBrains Runtime, or any
    /// `jlink --compress` image) is wrapped in a 29-byte `CompressedResourceHeader` (little-endian
    /// magic `0xCAFEFAFA`, then `size`/`uncompressed_size` i64s, decompressor name/config offsets, an
    /// `is_terminal` byte) before a zlib Deflate stream; inflate it. The `compressed` flag is set by
    /// the indexer only when a decompressor is named, so a resource without the header magic is
    /// returned as-is rather than mis-inflated.
    pub(super) fn read_entry(&self, location: (u64, usize, bool)) -> Option<Vec<u8>> {
        read_resource(&self.path, location.0, location.1, location.2)
    }

    /// Project the completed location index into the package view without copying its name tree.
    pub(super) fn package_catalog(&self) -> JarPackages {
        let mut catalog = JarPackages::with_names(Arc::clone(&self.names));
        for class in self.locations.class_ids(&self.names) {
            let Some(package) = self.names.parent(class) else {
                continue;
            };
            catalog.add_class(class);
            catalog.packages.entry(package).or_default().has_classes = true;
        }
        catalog.complete = !self.locations.is_empty();
        catalog
    }
}

/// Process-global jimage indexes, keyed by the exact entry snapshot. A JDK image is immutable for
/// the lifetime represented by its `EntryKey`; parsing its location table once avoids a 146 MB scan
/// for every compiler worker while the index and its shared name tree remain under one owner.
fn global_cache() -> &'static Mutex<HashMap<EntryKey, Arc<JimageIndex>>> {
    static CACHE: OnceLock<Mutex<HashMap<EntryKey, Arc<JimageIndex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::default()))
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

fn read_resource(path: &Path, offset: u64, size: usize, compressed: bool) -> Option<Vec<u8>> {
    use std::io::SeekFrom;
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; size];
    file.read_exact(&mut buf).ok()?;
    if compressed && buf.len() >= 29 && buf[0..4] == [0xFA, 0xFA, 0xFE, 0xCA] {
        let uncompressed = u64::from_le_bytes(buf[12..20].try_into().ok()?) as usize;
        // The jimage is a trusted local JDK file, but cap the pre-allocation hint anyway: a real
        // `.class` is far under this, and `read_to_end` grows past it if ever needed.
        let mut out = Vec::with_capacity(uncompressed.min(16 * 1024 * 1024));
        flate2::read::ZlibDecoder::new(&buf[29..])
            .read_to_end(&mut out)
            .ok()?;
        return Some(out);
    }
    Some(buf)
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
    // ImageLocation attributes: 1=MODULE, 2=PARENT, 3=BASE, 4=EXTENSION, 5=OFFSET,
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

    let mut index = JimageIndex {
        path: path.to_path_buf(),
        ..JimageIndex::default()
    };
    let mut module_ids: HashMap<&str, u32> = HashMap::default();
    index.locations.reserve_classes(table_length);
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
        if base.is_empty() {
            continue;
        }
        let (offset, compressed, uncompressed) = (attributes[5], attributes[6], attributes[7]);
        let stored = if compressed != 0 {
            compressed
        } else {
            uncompressed
        };
        let module_name = read_string(attributes[1]);
        let module = (!module_name.is_empty()).then(|| {
            *module_ids.entry(module_name).or_insert_with(|| {
                index.modules.push(JimageModule::new(module_name));
                (index.modules.len() - 1) as u32
            })
        });
        if parent.is_empty() {
            // `/<module>/module-info.class`: the module descriptor, not a class of any package.
            if let (Some(module), "module-info") = (module, base) {
                index.modules[module as usize].set_descriptor(
                    (content + offset) as u64,
                    stored,
                    compressed != 0,
                );
            }
            continue;
        }
        let class = class_id(&index.names, parent, base);
        if let (Some(module), Some(package)) = (module, index.names.parent(class)) {
            index.package_modules.entry(package).or_insert(module);
        }
        index
            .locations
            .insert(class, (content + offset) as u64, stored, compressed != 0);
    }
    // Growth kept every superseded child table so a probe still inside the old one stays valid.
    // This build is the only user of the tree, so those tables are just retained capacity.
    Arc::get_mut(&mut index.names)
        .expect("jimage name tree is private until publication")
        .reclaim_retired_tables();
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_catalog_share_exact_class_and_package_identities() {
        let mut index = JimageIndex::default();
        let class = class_id(&index.names, "sample/catalog", "Widget");
        index.locations.insert(class, 1, 2, false);
        index.locations.insert(class, 9, 8, true);

        assert_eq!(index.entry("sample/catalog/Widget"), Some((1, 2, false)));
        assert_eq!(index.locations.class_count(), 1);
        let package = index.names.parent(class).expect("class package");
        assert_eq!(index.locations.get(package), None);
        let catalog = index.package_catalog();

        assert!(Arc::ptr_eq(&catalog.names, &index.names));
        assert_eq!(catalog.classes, vec![class]);
        assert!(catalog.packages[&package].has_classes);
        assert_eq!(catalog.names.get("sample/catalog/Widget"), Some(class));
        assert_eq!(catalog.names.get("sample/catalog"), Some(package));
    }
}
