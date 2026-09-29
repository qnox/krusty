//! Shared storage for repeated JVM member and metadata-declaration spellings.
//!
//! These borrowed strings reduce duplicate storage; pointer equality is not a semantic member
//! identity. Resolution continues to use the declaration identities and candidate records owned by
//! the frontend.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{OnceLock, RwLock};

use crate::name_tree::{FxBuildHasher, FxHasher};

const SHARD_COUNT: usize = 64;
type SpellingShard = RwLock<HashSet<&'static str, FxBuildHasher>>;

fn shard(spelling: &str) -> &'static SpellingShard {
    static SPELLINGS: OnceLock<[SpellingShard; SHARD_COUNT]> = OnceLock::new();
    let mut hash = FxHasher::default();
    spelling.hash(&mut hash);
    &SPELLINGS.get_or_init(|| std::array::from_fn(|_| RwLock::new(HashSet::default())))
        [hash.finish() as usize % SHARD_COUNT]
}

pub(super) fn intern_owned(spelling: String) -> &'static str {
    let shard = shard(&spelling);
    if let Some(&existing) = shard.read().unwrap().get(spelling.as_str()) {
        return existing;
    }

    let mut spellings = shard.write().unwrap();
    if let Some(&existing) = spellings.get(spelling.as_str()) {
        return existing;
    }
    let stored = Box::leak(spelling.into_boxed_str());
    spellings.insert(stored);
    stored
}

pub(super) fn intern(spelling: &str) -> &'static str {
    let shard = shard(spelling);
    if let Some(&existing) = shard.read().unwrap().get(spelling) {
        return existing;
    }

    let mut spellings = shard.write().unwrap();
    if let Some(&existing) = spellings.get(spelling) {
        return existing;
    }
    let stored = Box::leak(spelling.to_owned().into_boxed_str());
    spellings.insert(stored);
    stored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_and_borrowed_spellings_share_storage() {
        let borrowed = intern("metadataMember");
        let owned = intern_owned("metadataMember".to_owned());
        assert!(std::ptr::eq(borrowed, owned));
    }

    #[test]
    fn concurrent_equal_spellings_share_storage() {
        let pointers = std::thread::scope(|scope| {
            (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        intern_owned("concurrentMetadataMember".to_owned()).as_ptr() as usize
                    })
                })
                .map(|thread| thread.join().expect("spelling worker"))
                .collect::<Vec<_>>()
        });
        assert!(pointers.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
