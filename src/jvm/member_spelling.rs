//! Catalog-owned storage for repeated JVM member and metadata spellings.
//!
//! A [`SpellingPool`] belongs to one classpath entry or one decode. Records keep [`Arc<str>`]
//! handles, so dropping the pool and the records reclaims the text. Pointer equality is storage
//! reuse, not semantic member identity.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::name_tree::{FxBuildHasher, FxHasher};

const SHARD_COUNT: usize = 64;

/// One catalog-owned spelling. Equality compares the text; [`MemberSpelling::ptr_eq`] is storage reuse.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct MemberSpelling(Arc<str>);

impl MemberSpelling {
    #[cfg(test)]
    pub(crate) fn ptr_eq(left: &Self, right: &Self) -> bool {
        Arc::ptr_eq(&left.0, &right.0)
    }

    #[cfg(test)]
    pub(crate) fn downgrade(this: &Self) -> std::sync::Weak<str> {
        Arc::downgrade(&this.0)
    }
}

impl Deref for MemberSpelling {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for MemberSpelling {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for MemberSpelling {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for MemberSpelling {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for MemberSpelling {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

impl From<MemberSpelling> for String {
    fn from(value: MemberSpelling) -> Self {
        value.0.to_string()
    }
}

impl From<&MemberSpelling> for String {
    fn from(value: &MemberSpelling) -> Self {
        value.0.to_string()
    }
}

impl PartialEq<str> for MemberSpelling {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for MemberSpelling {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl PartialEq<String> for MemberSpelling {
    fn eq(&self, other: &String) -> bool {
        self.0.as_ref() == other
    }
}

impl PartialEq<MemberSpelling> for str {
    fn eq(&self, other: &MemberSpelling) -> bool {
        self == other.0.as_ref()
    }
}

impl PartialEq<MemberSpelling> for &str {
    fn eq(&self, other: &MemberSpelling) -> bool {
        *self == other.0.as_ref()
    }
}

impl std::fmt::Display for MemberSpelling {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reclaimable intern table for one catalog or one decode.
pub(super) struct SpellingPool {
    shards: [Mutex<HashSet<MemberSpelling, FxBuildHasher>>; SHARD_COUNT],
    retained_bytes: AtomicUsize,
}

impl Default for SpellingPool {
    fn default() -> Self {
        Self::new()
    }
}

impl SpellingPool {
    pub(super) fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::new(HashSet::default())),
            retained_bytes: AtomicUsize::new(0),
        }
    }

    pub(super) fn intern(&self, spelling: &str) -> MemberSpelling {
        let shard = self.shard(spelling);
        if let Some(existing) = shard.lock().unwrap().get(spelling) {
            return existing.clone();
        }
        let mut spellings = shard.lock().unwrap();
        if let Some(existing) = spellings.get(spelling) {
            return existing.clone();
        }
        let stored = MemberSpelling::from(spelling);
        self.retained_bytes
            .fetch_add(spelling.len(), Ordering::Relaxed);
        spellings.insert(stored.clone());
        stored
    }

    pub(super) fn intern_owned(&self, spelling: String) -> MemberSpelling {
        let shard = self.shard(&spelling);
        let mut spellings = shard.lock().unwrap();
        if let Some(existing) = spellings.get(spelling.as_str()) {
            return existing.clone();
        }
        let stored = MemberSpelling::from(spelling);
        self.retained_bytes
            .fetch_add(stored.len(), Ordering::Relaxed);
        spellings.insert(stored.clone());
        stored
    }

    /// Bytes of distinct spellings retained by this pool. A repeated intern does not grow it.
    #[cfg(test)]
    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes.load(Ordering::Relaxed)
    }

    fn shard(&self, spelling: &str) -> &Mutex<HashSet<MemberSpelling, FxBuildHasher>> {
        let mut hash = FxHasher::default();
        spelling.hash(&mut hash);
        &self.shards[hash.finish() as usize % SHARD_COUNT]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_catalog_stores_a_repeated_spelling_once() {
        let catalog = SpellingPool::new();
        let borrowed = catalog.intern("metadataMember");
        let owned = catalog.intern_owned("metadataMember".to_owned());
        assert!(MemberSpelling::ptr_eq(&borrowed, &owned));
        assert_eq!(catalog.retained_bytes(), "metadataMember".len());
    }

    #[test]
    fn concurrent_equal_spellings_share_storage() {
        let catalog = SpellingPool::new();
        let pointers = std::thread::scope(|scope| {
            (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        MemberSpelling::ptr_eq(
                            &catalog.intern_owned("concurrentMetadataMember".to_owned()),
                            &catalog.intern("concurrentMetadataMember"),
                        )
                    })
                })
                .map(|thread| thread.join().expect("spelling worker"))
                .collect::<Vec<_>>()
        });
        assert!(pointers.iter().all(|shared| *shared));
        assert_eq!(catalog.retained_bytes(), "concurrentMetadataMember".len());
    }

    #[test]
    fn dropping_a_catalog_releases_spellings_a_later_catalog_recreates() {
        let spelling = "catalog-churn-member-name";
        let weak = {
            let catalog = SpellingPool::new();
            let retained = catalog.intern(spelling);
            let again = catalog.intern_owned(spelling.to_owned());
            assert!(MemberSpelling::ptr_eq(&retained, &again));
            assert_eq!(catalog.retained_bytes(), spelling.len());
            MemberSpelling::downgrade(&retained)
        };
        assert!(weak.upgrade().is_none());

        let later = SpellingPool::new();
        let recreated = later.intern(spelling);
        assert!(weak.upgrade().is_none());
        assert_eq!(&*recreated, spelling);
        assert_eq!(later.retained_bytes(), spelling.len());
    }
}
