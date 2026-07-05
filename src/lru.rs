//! A small count-bounded LRU cache — dependency-free, for the classpath's grow-only memoization caches.
//! Hot entries (the common stdlib/JDK classes queried on every compile) stay resident; cold one-off
//! entries evict once the cap is reached, so memory plateaus instead of growing toward the full JDK.
//!
//! Recency is a monotonically increasing tick stamped on each access; eviction removes the entry with
//! the smallest tick (a linear scan, run only when inserting a NEW key into a full cache — rare relative
//! to hits once the working set is warm). The count cap defaults per cache and is overridable for all
//! caches at once via the `KRUSTY_CACHE_CAP` environment variable (for profiling / constrained hosts).

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

pub struct LruCache<K, V> {
    cap: usize,
    /// Monotonic recency counter — a `u64` bumped on every access. At even a billion accesses per second
    /// it takes ~580 years to wrap, so overflow (which would invert eviction order) is unreachable in any
    /// real run; a `u64` avoids the pinning `saturating_add` would cause at the ceiling.
    tick: u64,
    map: HashMap<K, (V, u64)>,
}

impl<K: Eq + Hash + Clone, V> LruCache<K, V> {
    /// A cache bounded to `default_cap` entries, or to `KRUSTY_CACHE_CAP` when that env var is set.
    pub fn new(default_cap: usize) -> Self {
        let cap = std::env::var("KRUSTY_CACHE_CAP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(default_cap)
            .max(1);
        LruCache {
            cap,
            tick: 0,
            map: HashMap::new(),
        }
    }

    /// Read `k`, marking it most-recently-used. `None` if absent (the caller recomputes and `insert`s).
    /// Accepts any borrowed key form (`&str` for a `String` key), like [`HashMap::get`].
    pub fn get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.tick += 1;
        let t = self.tick;
        let e = self.map.get_mut(k)?;
        e.1 = t;
        Some(&e.0)
    }

    pub fn contains_key<Q>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.contains_key(k)
    }

    /// Insert (or replace) `k`, evicting the least-recently-used entry first if a NEW key would exceed
    /// the cap. Replacing an existing key never evicts.
    pub fn insert(&mut self, k: K, v: V) {
        self.tick += 1;
        let t = self.tick;
        if self.map.len() >= self.cap && !self.map.contains_key(&k) {
            if let Some(lru) = self
                .map
                .iter()
                .min_by_key(|(_, (_, stamp))| *stamp)
                .map(|(key, _)| key.clone())
            {
                self.map.remove(&lru);
            }
        }
        self.map.insert(k, (v, t));
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl<K: Eq + Hash + Clone, V> Default for LruCache<K, V> {
    /// A modestly-sized cache — enough to keep a warm working set of common classes/queries resident.
    /// Callers with a known access profile pass an explicit cap via [`LruCache::new`].
    fn default() -> Self {
        Self::new(4096)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_when_full() {
        let mut c = LruCache::new(2);
        c.insert("a", 1);
        c.insert("b", 2);
        // Touch `a` so `b` becomes the LRU.
        assert_eq!(c.get(&"a"), Some(&1));
        c.insert("c", 3); // evicts `b`
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(&"b"), None);
        assert_eq!(c.get(&"a"), Some(&1));
        assert_eq!(c.get(&"c"), Some(&3));
    }

    #[test]
    fn replacing_existing_key_does_not_evict() {
        let mut c = LruCache::new(2);
        c.insert("a", 1);
        c.insert("b", 2);
        c.insert("a", 10); // replace, not a new key
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(&"a"), Some(&10));
        assert_eq!(c.get(&"b"), Some(&2));
    }

    #[test]
    fn env_cap_overrides_default() {
        // Not asserting the env path (global process state); just the default-cap behaviour.
        let mut c: LruCache<u32, u32> = LruCache::new(1);
        c.insert(1, 1);
        c.insert(2, 2);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(&1), None);
        assert_eq!(c.get(&2), Some(&2));
    }
}
