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

/// The releases a top-level `ct.sym` folder serves: one base-36 digit per release (`7`, `8`, `A`
/// for 10, ...). A `-modules` suffix marks the folder holding those releases' module descriptors in
/// the layout of JDKs before 12.
fn folder_releases(folder: &str) -> impl Iterator<Item = u8> + '_ {
    let set = folder
        .rsplit_once("-modules")
        .map_or(folder, |(set, _)| set);
    set.chars()
        .filter_map(|digit| digit.to_digit(36))
        .filter_map(|release| u8::try_from(release).ok())
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
    let Some(mut archive) = File::open(path)
        .ok()
        .and_then(|file| crate::archive::open_file(file).ok())
    else {
        return Ok(None);
    };
    index_archive(&mut archive, path, release)
}

/// The JDK releases whose API the `ct.sym` at `path` describes, ascending, or `None` when it does
/// not open as an archive. The newest of them is the release of the JDK that ships the file, whose
/// own API is its module image.
pub(crate) fn ct_sym_releases(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let Some(archive) = File::open(path)
        .ok()
        .and_then(|file| crate::archive::open_file(file).ok())
    else {
        return Ok(None);
    };
    archive_releases(&archive, path)
        .map(Some)
        .map_err(|error| error.diagnostic())
}

/// The releases the archive's top-level folders name, ascending.
fn archive_releases(
    archive: &impl ArchiveEntries,
    path: &Path,
) -> Result<Vec<u8>, ClassMetadataLoadError> {
    let mut releases = Vec::new();
    for ordinal in 0..archive.entry_count() {
        let name = archive_entry_name(archive, path, ordinal)?;
        let folder = name.split('/').next().unwrap_or_default();
        if folder != "META-INF" {
            releases.extend(folder_releases(folder));
        }
    }
    releases.sort_unstable();
    releases.dedup();
    Ok(releases)
}

/// Build one release's public-class index. Entries are classfiles stored with a `.sig` suffix under
/// a release-set folder, which names every release for which those exact bytes apply. A JDK from 12
/// on (the newest release the archive names) stores them as `<release-set>/<module>/<internal>.sig`;
/// an older one as `<release-set>/<internal>.sig`, with its module descriptors apart in
/// `<release-set>-modules/` folders. kotlinc reads the archive the same way. A name that does not
/// decode fails the build: the entry cannot be classified, so the view would lack its class.
fn index_archive(
    archive: &mut impl ArchiveEntries,
    path: &Path,
    release: u8,
) -> Result<Option<CtSymIndex>, ClassMetadataLoadError> {
    let module_folders = archive_releases(archive, path)?
        .last()
        .is_some_and(|&newest| newest >= 12);
    let mut index = CtSymIndex::default();
    for ordinal in 0..archive.entry_count() {
        let name = archive_entry_name(archive, path, ordinal)?;
        let Some((folder, rest)) = name.split_once('/') else {
            continue;
        };
        if folder == "META-INF"
            || folder.ends_with("-modules")
            || !folder_releases(folder).any(|served| served == release)
        {
            continue;
        }
        // A name with no module segment in the module layout is not a class (`L/`,
        // `L/system-modules`). Skip it. Returning from the loop on that name discarded every
        // class already recorded, so the selected JDK release view was empty.
        let resource = if module_folders {
            let Some((_module, resource)) = rest.split_once('/') else {
                continue;
            };
            resource
        } else {
            rest
        };
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
            .or_insert_with(|| name.to_string());
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
            index_archive(&mut symbols, path, 8)
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

    fn indexed(index: &CtSymIndex) -> Vec<(String, String)> {
        let mut names = index
            .by_name
            .iter()
            .map(|(&name, entry)| (index.names.render(name), entry.clone()))
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// From JDK 12 on, each release-set folder holds module folders; release 7 and older are
    /// base-36 digits like any other.
    #[test]
    fn the_module_layout_indexes_each_release_below_its_module() {
        use super::super::archive_entries::test_archives::archive;
        let path = Path::new("/jdk17/lib/ct.sym");
        let mut symbols = archive(&[
            ("789ABCDEFGH/java.base/java/lang/Object.sig", b""),
            ("7/java.base/java/nio/ByteBuffer.sig", b""),
            ("89/java.base/java/nio/ByteBuffer.sig", b""),
            ("ABCDEFGH/java.base/module-info.sig", b""),
            ("H/system-modules", b""),
        ]);
        let index = index_archive(&mut symbols, path, 7)
            .expect("index")
            .expect("release 7 view");
        assert_eq!(
            indexed(&index),
            [
                (
                    "java/lang/Object".to_string(),
                    "789ABCDEFGH/java.base/java/lang/Object.sig".to_string()
                ),
                (
                    "java/nio/ByteBuffer".to_string(),
                    "7/java.base/java/nio/ByteBuffer.sig".to_string()
                ),
            ]
        );
    }

    /// Before JDK 12, classes sit directly under the release-set folder, and the module
    /// descriptors sit apart under `<release-set>-modules`.
    #[test]
    fn the_layout_before_jdk_12_indexes_classes_directly_below_the_release_set() {
        use super::super::archive_entries::test_archives::archive;
        let path = Path::new("/jdk11/lib/ct.sym");
        let mut symbols = archive(&[
            ("6789AB/java/lang/Object.sig", b""),
            ("876/java/nio/ByteBuffer.sig", b""),
            ("9AB/java/nio/ByteBuffer.sig", b""),
            ("9AB-modules/java.base/module-info.sig", b""),
        ]);
        let index = index_archive(&mut symbols, path, 8)
            .expect("index")
            .expect("release 8 view");
        assert_eq!(
            indexed(&index),
            [
                (
                    "java/lang/Object".to_string(),
                    "6789AB/java/lang/Object.sig".to_string()
                ),
                (
                    "java/nio/ByteBuffer".to_string(),
                    "876/java/nio/ByteBuffer.sig".to_string()
                ),
            ]
        );
        let index = index_archive(&mut symbols, path, 10)
            .expect("index")
            .expect("release 10 view");
        assert_eq!(
            indexed(&index),
            [
                (
                    "java/lang/Object".to_string(),
                    "6789AB/java/lang/Object.sig".to_string()
                ),
                (
                    "java/nio/ByteBuffer".to_string(),
                    "9AB/java/nio/ByteBuffer.sig".to_string()
                ),
            ]
        );
    }
}
