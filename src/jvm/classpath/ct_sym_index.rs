//! Release-specific class indexes for the JDK `lib/ct.sym` public API archive.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::name_tree::{NameId, NameTree};

use super::{entry_stamp, EntryKey};

/// Semantic internal name → physical zip entry name. A single signature entry may serve several
/// releases (`89A/...`); filtering happens here so ordinary class lookup never needs to understand
/// the archive layout.
#[derive(Default, Debug)]
pub(super) struct CtSymIndex {
    pub(super) names: NameTree,
    pub(super) by_name: HashMap<NameId, String>,
}

fn release_symbol(release: u8) -> Option<u8> {
    match release {
        8 | 9 => Some(b'0' + release),
        10..=35 => Some(b'A' + (release - 10)),
        _ => None,
    }
}

fn global_cache() -> &'static Mutex<HashMap<EntryKey, Arc<CtSymIndex>>> {
    static CACHE: OnceLock<Mutex<HashMap<EntryKey, Arc<CtSymIndex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn cached_ct_sym_index(path: &Path, release: u8) -> Option<Arc<CtSymIndex>> {
    let key = EntryKey {
        path: path.to_path_buf(),
        stamp: entry_stamp(path),
        jdk_release: Some(release),
    };
    let mut cache = global_cache().lock().unwrap();
    if let Some(index) = cache.get(&key) {
        return Some(index.clone());
    }
    let index = Arc::new(build_ct_sym_index(path, release)?);
    cache.insert(key, index.clone());
    Some(index)
}

/// Build one release's public-class index. Entries are classfiles stored with a `.sig` suffix under
/// `<release-set>/<module>/<internal>.sig`; the release-set contains every release for which those
/// exact bytes apply.
pub(super) fn build_ct_sym_index(path: &Path, release: u8) -> Option<CtSymIndex> {
    let release = release_symbol(release)?;
    let file = File::open(path).ok()?;
    let archive = zip::ZipArchive::new(file).ok()?;
    let mut index = CtSymIndex::default();
    for ordinal in 0..archive.len() {
        // A central-directory name with no module segment is not a class (`L/`,
        // `L/system-modules`). Skip it. Returning from the loop on that name discarded every
        // class already recorded, so the selected JDK release view was empty.
        let Some(name) = archive.name_for_index(ordinal) else {
            continue;
        };
        let mut segments = name.splitn(3, '/');
        let (Some(releases), Some(_module), Some(resource)) =
            (segments.next(), segments.next(), segments.next())
        else {
            continue;
        };
        if !releases.as_bytes().contains(&release) {
            continue;
        }
        let Some(internal) = resource.strip_suffix(".sig") else {
            continue;
        };
        if internal == "module-info" || internal.ends_with("/module-info") {
            continue;
        }
        let internal = index.names.insert(internal);
        index
            .by_name
            .entry(internal)
            .or_insert_with(|| name.to_owned());
    }
    (!index.by_name.is_empty()).then_some(index)
}
