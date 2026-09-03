//! Concurrent, append-only storage for the compact borrowed pieces of [`super::Ty`].
//!
//! Type construction is read-mostly after the first occurrence of a shape. Keeping one global
//! mutex around each interner serializes otherwise independent frontend workers, so the value hash
//! selects a shard and existing values are read under a shared lock. The write path repeats the
//! lookup after acquiring exclusivity because another worker may have inserted the value between
//! the two locks.

use crate::name_tree::{FxBuildHasher, FxHasher};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;

const SHARD_COUNT: usize = 64;

pub(super) struct ShardedInterner<T: ?Sized + Eq + Hash + 'static> {
    shards: [RwLock<HashSet<&'static T, FxBuildHasher>>; SHARD_COUNT],
}

impl<T: ?Sized + Eq + Hash + 'static> Default for ShardedInterner<T> {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(HashSet::default())),
        }
    }
}

impl<T: ?Sized + Eq + Hash + 'static> ShardedInterner<T> {
    #[inline]
    fn shard(&self, value: &T) -> &RwLock<HashSet<&'static T, FxBuildHasher>> {
        let mut hash = FxHasher::default();
        value.hash(&mut hash);
        &self.shards[hash.finish() as usize % SHARD_COUNT]
    }

    pub(super) fn intern_ref_with(
        &self,
        value: &T,
        allocate: impl FnOnce(&T) -> &'static T,
    ) -> &'static T {
        let shard = self.shard(value);
        if let Some(&existing) = shard.read().unwrap().get(value) {
            return existing;
        }

        let mut values = shard.write().unwrap();
        if let Some(&existing) = values.get(value) {
            return existing;
        }
        let stored = allocate(value);
        values.insert(stored);
        stored
    }
}

impl<T: Eq + Hash + 'static> ShardedInterner<T> {
    pub(super) fn intern_owned(&self, value: T) -> &'static T {
        let shard = self.shard(&value);
        if let Some(&existing) = shard.read().unwrap().get(&value) {
            return existing;
        }

        let mut values = shard.write().unwrap();
        if let Some(&existing) = values.get(&value) {
            return existing;
        }
        let stored = Box::leak(Box::new(value));
        values.insert(stored);
        stored
    }
}

#[cfg(test)]
mod tests {
    use super::ShardedInterner;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_equal_values_share_one_allocation() {
        let interner = Arc::new(ShardedInterner::<String>::default());
        let barrier = Arc::new(Barrier::new(8));
        let pointers = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| {
                    let interner = Arc::clone(&interner);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        interner.intern_owned("same type shape".to_string()) as *const String
                            as usize
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert!(pointers.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn borrowed_values_are_canonicalized_by_value() {
        let interner = ShardedInterner::<str>::default();
        let first =
            interner.intern_ref_with("T", |value| Box::leak(value.to_owned().into_boxed_str()));
        let second = interner.intern_ref_with(&"T".to_string(), |value| {
            Box::leak(value.to_owned().into_boxed_str())
        });

        assert!(std::ptr::eq(first, second));
    }
}
