//! Decoded IR declaration trees shared by every compilation that opens the same KLIB generation.
//!
//! An archive's ordinary IR channel decodes to the same trees whichever compilation opens it and
//! whatever else that compilation reads, so the decode is kept per archive content, as the JVM
//! classpath keeps each entry's own facts. The key is the content fingerprint of the bytes the open
//! froze, never a path or a file stamp: a replaced archive has another fingerprint and decodes
//! afresh. Facts that depend on the whole input set (stdlib authority, defaults) are still built
//! by each open. Prepared-inline trees are decoded only for the inline declarations the input set
//! selects, so they are kept per generation and exact selection.

use std::sync::{Arc, Mutex, OnceLock};

use crate::fx_hash::HashSet;
use crate::klib::{KlibArchive, KlibError};
use crate::lru::LruCache;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::{
    read_declaration_trees, read_inlinable_declaration_trees_where, KlibIrDecodeError,
    KlibIrModuleTrees, KlibIrSignature,
};

/// Archives whose trees stay resident: the platform stdlib and the dependencies of recent
/// compilations. Each entry is one archive's whole ordinary IR, so the bound is a count.
const RESIDENT_ARCHIVES: usize = 32;

/// Why an archive's ordinary IR trees are unavailable.
#[derive(Debug)]
pub(super) enum SharedTreesError {
    /// The archive's content could not be read to identify its generation.
    Fingerprint(KlibError),
    /// The archive's IR channel does not decode.
    Decode(KlibIrDecodeError),
}

/// Selections of one archive's prepared-inline trees kept at once. An input set selects the same
/// inline declarations of an archive unless another input withdraws one, so one usually suffices.
const RESIDENT_INLINE_SELECTIONS: usize = 2;

/// One archive's prepared-inline trees, decoded for exactly `selected`.
struct InlineSelection {
    selected: HashSet<KlibPublicIdSignature>,
    trees: Arc<KlibIrModuleTrees>,
}

/// Decoded IR trees by the content fingerprint of the archive they were decoded from.
pub(super) struct SharedTrees {
    decoded: Mutex<LruCache<[u8; 32], Arc<KlibIrModuleTrees>>>,
    inline: Mutex<LruCache<[u8; 32], Vec<InlineSelection>>>,
}

impl SharedTrees {
    pub(super) fn new(resident: usize) -> Self {
        Self {
            decoded: Mutex::new(LruCache::new_fixed(resident)),
            inline: Mutex::new(LruCache::new_fixed(resident)),
        }
    }

    /// The trees every compilation in this process shares.
    pub(super) fn process() -> &'static Self {
        static PROCESS: OnceLock<SharedTrees> = OnceLock::new();
        PROCESS.get_or_init(|| Self::new(RESIDENT_ARCHIVES))
    }

    /// The ordinary IR trees of `archive`, decoded once per content generation. A failed decode
    /// is not kept, so the next open reports it again.
    pub(super) fn main_declaration_trees(
        &self,
        archive: &KlibArchive,
    ) -> Result<Arc<KlibIrModuleTrees>, SharedTreesError> {
        let fingerprint = archive
            .content_fingerprint()
            .map_err(SharedTreesError::Fingerprint)?;
        if let Some(trees) = self.decoded().get(&fingerprint) {
            return Ok(Arc::clone(trees));
        }
        let trees = Arc::new(read_declaration_trees(archive).map_err(SharedTreesError::Decode)?);
        self.decoded().insert(fingerprint, Arc::clone(&trees));
        Ok(trees)
    }

    /// The prepared-inline trees of `archive` for exactly the `selected` public signatures,
    /// decoded once per content generation and selection. A failed decode is not kept.
    pub(super) fn inline_declaration_trees(
        &self,
        archive: &KlibArchive,
        selected: &HashSet<KlibPublicIdSignature>,
    ) -> Result<Arc<KlibIrModuleTrees>, SharedTreesError> {
        let fingerprint = archive
            .content_fingerprint()
            .map_err(SharedTreesError::Fingerprint)?;
        if let Some(kept) = self.inline().get(&fingerprint).and_then(|selections| {
            selections
                .iter()
                .find(|selection| &selection.selected == selected)
                .map(|selection| Arc::clone(&selection.trees))
        }) {
            return Ok(kept);
        }
        let trees = Arc::new(
            read_inlinable_declaration_trees_where(archive, |signature| {
                matches!(signature, KlibIrSignature::Public(signature) if selected.contains(signature))
            })
            .map_err(SharedTreesError::Decode)?,
        );
        let mut inline = self.inline();
        let mut selections = inline.remove(&fingerprint).unwrap_or_default();
        selections.retain(|selection| &selection.selected != selected);
        selections.truncate(RESIDENT_INLINE_SELECTIONS - 1);
        selections.insert(
            0,
            InlineSelection {
                selected: selected.clone(),
                trees: Arc::clone(&trees),
            },
        );
        inline.insert(fingerprint, selections);
        Ok(trees)
    }

    fn inline(&self) -> std::sync::MutexGuard<'_, LruCache<[u8; 32], Vec<InlineSelection>>> {
        self.inline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn decoded(&self) -> std::sync::MutexGuard<'_, LruCache<[u8; 32], Arc<KlibIrModuleTrees>>> {
        // A panic while the lock is held cannot leave a half-written entry: insertion is one call.
        self.decoded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::SharedTrees;
    use crate::klib::KlibArchive;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn scratch_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "krusty-shared-trees-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn copy_to_scratch(source: &Path, label: &str) -> PathBuf {
        let copy = scratch_path(label).with_extension("klib");
        std::fs::copy(source, &copy).expect("copy the fixture KLIB");
        copy
    }

    #[test]
    fn opens_of_one_generation_share_its_decoded_trees() {
        let shared = SharedTrees::new(4);
        let path = fixture("klib_signatures/signatures.klib");
        let first = shared
            .main_declaration_trees(&KlibArchive::open(&path).expect("open"))
            .expect("decode");
        let second = shared
            .main_declaration_trees(&KlibArchive::open(&path).expect("reopen"))
            .expect("decode again");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.trees().len(),
            crate::metadata::klib_ir::read_declaration_trees(
                &KlibArchive::open(&path).expect("open for a direct decode")
            )
            .expect("direct decode")
            .trees()
            .len()
        );
    }

    #[test]
    fn the_same_bytes_at_another_path_share_their_decoded_trees() {
        let shared = SharedTrees::new(4);
        let path = fixture("klib_signatures/signatures.klib");
        let copy = copy_to_scratch(&path, "copy");
        let original = shared
            .main_declaration_trees(&KlibArchive::open(&path).expect("open"))
            .expect("decode");
        let copied = shared
            .main_declaration_trees(&KlibArchive::open(&copy).expect("open the copy"))
            .expect("decode the copy");
        assert!(Arc::ptr_eq(&original, &copied));
        std::fs::remove_file(copy).expect("remove the copy");
    }

    /// Unpack the fixture so a test can rewrite one of its entries in place.
    fn unpacked_fixture(label: &str) -> PathBuf {
        let directory = scratch_path(label);
        let file = std::fs::File::open(fixture("klib_signatures/signatures.klib")).expect("open");
        zip::ZipArchive::new(file)
            .expect("read the fixture zip")
            .extract(&directory)
            .expect("unpack the fixture");
        directory
    }

    #[test]
    fn a_rewritten_archive_decodes_its_own_generation() {
        let shared = SharedTrees::new(4);
        let directory = unpacked_fixture("rewritten");
        let before = shared
            .main_declaration_trees(&KlibArchive::open(&directory).expect("open"))
            .expect("decode");
        let manifest = directory.join("default/manifest");
        let mut text = std::fs::read_to_string(&manifest).expect("read the manifest");
        text.push_str("krusty_test_generation=2\n");
        std::fs::write(&manifest, text).expect("rewrite the manifest in place");
        let after = shared
            .main_declaration_trees(&KlibArchive::open(&directory).expect("reopen"))
            .expect("decode the rewritten generation");
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(before.trees().len(), after.trees().len());
        std::fs::remove_dir_all(directory).expect("remove the unpacked fixture");
    }
}
