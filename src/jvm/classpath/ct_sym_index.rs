//! Release-specific class indexes for the JDK `lib/ct.sym` public API archive.

use crate::fx_hash::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::name_tree::{NameId, NameTree};

use super::archive_entries::ArchiveEntries;
use super::metadata_indexes::{archive_entry_name, ClassMetadataLoadError};
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
    CACHE.get_or_init(|| Mutex::new(HashMap::default()))
}

/// The release view of the `ct.sym` at `path`, or `None` when it has none. A failure is returned
/// rather than cached or presented as a JDK without those classes.
pub(super) fn cached_ct_sym_index(
    path: &Path,
    release: u8,
) -> Result<Option<Arc<CtSymIndex>>, ClassMetadataLoadError> {
    let key = EntryKey {
        path: path.to_path_buf(),
        stamp: entry_stamp(path),
        jdk_release: Some(release),
    };
    cache_index(key, || build_ct_sym_index(path, release))
}

fn cache_index(
    key: EntryKey,
    build: impl FnOnce() -> Result<Option<CtSymIndex>, ClassMetadataLoadError>,
) -> Result<Option<Arc<CtSymIndex>>, ClassMetadataLoadError> {
    let mut cache = global_cache().lock().unwrap();
    if let Some(index) = cache.get(&key) {
        return Ok(Some(index.clone()));
    }
    let Some(index) = build()? else {
        return Ok(None);
    };
    let index = Arc::new(index);
    cache.insert(key, index.clone());
    Ok(Some(index))
}

fn build_ct_sym_index(
    path: &Path,
    release: u8,
) -> Result<Option<CtSymIndex>, ClassMetadataLoadError> {
    let Some(release) = release_symbol(release) else {
        return Ok(None);
    };
    let Some(mut archive) = File::open(path)
        .ok()
        .and_then(|file| zip::ZipArchive::new(file).ok())
    else {
        return Ok(None);
    };
    index_archive(&mut archive, path, release)
}

/// Build one release's public-class index. Entries are classfiles stored with a `.sig` suffix under
/// `<release-set>/<module>/<internal>.sig`; the release-set contains every release for which those
/// exact bytes apply. A name that does not decode fails the build: the entry cannot be classified,
/// so the view would lack its class.
fn index_archive(
    archive: &mut impl ArchiveEntries,
    path: &Path,
    release: u8,
) -> Result<Option<CtSymIndex>, ClassMetadataLoadError> {
    let mut index = CtSymIndex::default();
    for ordinal in 0..archive.entry_count() {
        let name = archive_entry_name(archive, path, ordinal)?;
        // A central-directory name with no module segment is not a class (`L/`,
        // `L/system-modules`). Skip it. Returning from the loop on that name discarded every
        // class already recorded, so the selected JDK release view was empty.
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
            .or_insert_with(|| name.into_owned());
    }
    Ok((!index.by_name.is_empty()).then_some(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioned_java8_ct_sym_indexes_public_classes() {
        let Some(symbols) = crate::toolchain::jdk_symbols() else {
            return;
        };
        if symbols.file_name().is_none_or(|name| name != "ct.sym") {
            return;
        }
        let index = build_ct_sym_index(&symbols, 8)
            .ok()
            .flatten()
            .expect("Java 8 ct.sym index");
        assert!(
            index.names.get("java/util/ArrayList").is_some(),
            "Java 8 ct.sym view indexed {} classes but not java.util.ArrayList",
            index.by_name.len()
        );
        assert!(
            index.by_name.len() > 1000,
            "Java 8 ct.sym view collapsed to {} classes",
            index.by_name.len()
        );
    }

    #[test]
    fn an_undecodable_name_after_a_class_fails_the_release_view_without_caching_it() {
        use super::super::archive_entries::test_archives::{archive, UndecodableName};
        let path = Path::new("/undecodable/lib/ct.sym");
        let key = EntryKey {
            path: path.to_path_buf(),
            stamp: None,
            jdk_release: Some(8),
        };
        let built = cache_index(key.clone(), || {
            let mut symbols = UndecodableName {
                inner: archive(&[
                    ("8/java.base/java/lang/Object.sig", b""),
                    ("8/java.base/java/lang/String.sig", b""),
                ]),
                index: 1,
            };
            index_archive(&mut symbols, path, b'8')
        });

        let Err(failure) = built else {
            panic!("a partial ct.sym view was published: {built:?}");
        };
        assert_eq!(
            failure.diagnostic(),
            "cannot read classpath entry /undecodable/lib/ct.sym: undecodable entry name: \
             i/o error: undecodable entry name"
        );
        assert!(!global_cache().lock().unwrap().contains_key(&key));
    }
}
