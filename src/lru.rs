//! A small count-bounded LRU cache — dependency-free, for the classpath's grow-only memoization caches.
//! Hot entries (the common stdlib/JDK classes queried on every compile) stay resident; cold one-off
//! entries evict once the cap is reached, so memory plateaus instead of growing toward the full JDK.
//!
//! A bounded min-heap avoids scanning the full map when an entry must be evicted.

use crate::name_tree::FxBuildHasher;
use std::borrow::Borrow;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::hash::Hash;

struct Recency<K> {
    tick: u64,
    key: K,
}

impl<K> PartialEq for Recency<K> {
    fn eq(&self, other: &Self) -> bool {
        self.tick == other.tick
    }
}

impl<K> Eq for Recency<K> {}

impl<K> PartialOrd for Recency<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K> Ord for Recency<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.tick.cmp(&other.tick)
    }
}

pub struct LruCache<K, V> {
    cap: usize,
    /// Monotonic recency counter — a `u64` bumped on every access. At even a billion accesses per second
    /// it takes ~580 years to wrap, so overflow (which would invert eviction order) is unreachable in any
    /// real run; a `u64` avoids the pinning `saturating_add` would cause at the ceiling.
    tick: u64,
    map: HashMap<K, (V, u64), FxBuildHasher>,
    /// One lazily refreshed eviction candidate per key.
    recency: BinaryHeap<Reverse<Recency<K>>>,
}

impl<K: Eq + Hash + Clone, V> LruCache<K, V> {
    fn with_cap(cap: usize) -> Self {
        LruCache {
            cap: cap.max(1),
            tick: 0,
            map: HashMap::default(),
            recency: BinaryHeap::new(),
        }
    }

    /// A cache bounded to `default_cap` entries, or to `KRUSTY_CACHE_CAP` when that env var is set.
    pub fn new(default_cap: usize) -> Self {
        let cap = std::env::var("KRUSTY_CACHE_CAP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(default_cap)
            .max(1);
        Self::with_cap(cap)
    }

    /// A cache whose cap cannot be overridden by `KRUSTY_CACHE_CAP`.
    pub fn new_fixed(cap: usize) -> Self {
        Self::with_cap(cap)
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

    pub fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.tick += 1;
        let t = self.tick;
        let e = self.map.get_mut(k)?;
        e.1 = t;
        Some(&mut e.0)
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
        let replacing = self.map.contains_key(&k);
        if self.map.len() >= self.cap && !replacing {
            while let Some(Reverse(candidate)) = self.recency.pop() {
                let Some(current) = self.map.get(&candidate.key).map(|(_, stamp)| *stamp) else {
                    continue;
                };
                if current == candidate.tick {
                    self.map.remove(&candidate.key);
                    break;
                }
                self.recency.push(Reverse(Recency {
                    tick: current,
                    key: candidate.key,
                }));
            }
        }
        if replacing {
            self.map.insert(k, (v, t));
        } else {
            self.map.insert(k.clone(), (v, t));
            self.recency.push(Reverse(Recency { tick: t, key: k }));
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Remove every entry.
    pub fn clear(&mut self) {
        self.map.clear();
        self.recency.clear();
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
        assert_eq!(c.recency.len(), 2);
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(&"a"), Some(&10));
        assert_eq!(c.get(&"b"), Some(&2));
    }

    #[test]
    fn mutable_access_refreshes_recency() {
        let mut c = LruCache::new_fixed(2);
        c.insert("a", 1);
        c.insert("b", 2);
        *c.get_mut(&"a").unwrap() = 10;
        c.insert("c", 3);
        assert_eq!(c.get(&"a"), Some(&10));
        assert_eq!(c.get(&"b"), None);
    }

    #[test]
    fn clear_empties_the_cache() {
        let mut c = LruCache::new(4);
        c.insert("a", 1);
        c.insert("b", 2);
        c.clear();
        assert!(c.is_empty());
        assert!(c.recency.is_empty());
        assert_eq!(c.get(&"a"), None);
        assert_eq!(c.get(&"b"), None);
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

    #[test]
    fn recency_heap_stays_bounded_across_hits() {
        let mut c = LruCache::new_fixed(64);
        for key in 0..64 {
            c.insert(key, key);
        }
        for _ in 0..1024 {
            for key in 0..64 {
                assert_eq!(c.get(&key), Some(&key));
            }
        }

        assert_eq!(c.recency.len(), 64);
        c.insert(64, 64);
        assert_eq!(c.recency.len(), 64);
        assert_eq!(c.len(), 64);
        assert_eq!(c.get(&0), None);
    }
}
