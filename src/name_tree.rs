/// FxHash (rustc's non-cryptographic hasher), hand-rolled to keep the crate dependency-lean. The
/// compiler's map keys are trusted internal data, so DoS-resistant SipHash buys nothing and costs
/// measurably on the hot name/id lookups.
#[derive(Default, Clone)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while rest.len() >= 8 {
            self.add(u64::from_le_bytes(rest[..8].try_into().unwrap()));
            rest = &rest[8..];
        }
        if rest.len() >= 4 {
            self.add(u64::from(u32::from_le_bytes(rest[..4].try_into().unwrap())));
            rest = &rest[4..];
        }
        for &b in rest {
            self.add(u64::from(b));
        }
    }
    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(u64::from(n));
    }
    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(u64::from(n));
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuildHasher = std::hash::BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct NameId(pub(crate) u32);

#[derive(Clone, Debug)]
struct NameNode {
    parent: Option<NameId>,
    sep: u8,
    segment: std::sync::Arc<str>,
}

/// Chunked append-only node storage. A node is written exactly once (by the single writer, before its
/// id is published through a child-table slot or returned to the caller) and never moves — chunks are
/// allocated up front per size class, so readers index without any lock or reallocation hazard.
const BASE_LOG2: u32 = 6;
const BASE: u32 = 1 << BASE_LOG2; // chunk 0 holds 64 nodes; chunk k holds 64 << k
const CHUNKS: usize = 26; // total capacity 64 * (2^26 - 1) > u32::MAX

struct Chunk(Box<[UnsafeCell<MaybeUninit<NameNode>>]>);

// The per-slot data is only written before the slot's id is published (release/acquire via the child
// table or the arena len) and is immutable afterwards.
unsafe impl Sync for Chunk {}
unsafe impl Send for Chunk {}

struct Arena {
    chunks: [OnceLock<Chunk>; CHUNKS],
    /// Number of initialized nodes. `Release`-published after the node is written; readers that index
    /// by an id obtained from a published slot need no check at all.
    len: AtomicU32,
}

/// chunk index + offset within the chunk for a node id.
#[inline]
fn locate(id: u32) -> (usize, usize) {
    let n = (id >> BASE_LOG2) + 1;
    let k = 31 - n.leading_zeros();
    let start = (BASE << k) - BASE;
    (k as usize, (id - start) as usize)
}

impl Arena {
    fn new() -> Self {
        Arena {
            chunks: std::array::from_fn(|_| OnceLock::new()),
            len: AtomicU32::new(0),
        }
    }

    #[inline]
    fn get(&self, id: u32) -> &NameNode {
        let (k, off) = locate(id);
        let chunk = self.chunks[k].get().expect("published node id has a chunk");
        // SAFETY: `id` was published (returned from `push` / read from a table slot), so the slot was
        // fully initialized before the publishing release store, and node slots are never mutated.
        unsafe { (*chunk.0[off].get()).assume_init_ref() }
    }

    /// Caller must be the unique writer (holds the tree's writer lock, or has exclusive access during
    /// construction).
    fn push(&self, node: NameNode) -> u32 {
        let id = self.len.load(Ordering::Relaxed);
        assert!(id < u32::MAX, "name tree node count exceeded u32 capacity");
        let (k, off) = locate(id);
        let chunk = self.chunks[k].get_or_init(|| {
            Chunk(
                (0..(BASE << k) as usize)
                    .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                    .collect(),
            )
        });
        // SAFETY: single writer; slot `off` in chunk `k` is uninitialized (len == id) and unaliased.
        unsafe { (*chunk.0[off].get()).write(node) };
        self.len.store(id + 1, Ordering::Release);
        id
    }

    fn len(&self) -> u32 {
        self.len.load(Ordering::Acquire)
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        for id in 0..self.len.load(Ordering::Acquire) {
            let (k, off) = locate(id);
            let chunk = self.chunks[k]
                .get_mut()
                .expect("initialized node has a chunk");
            // SAFETY: ids below len are initialized, dropped exactly once here.
            unsafe { (*chunk.0[off].get()).assume_init_drop() };
        }
    }
}

/// Open-addressing child index: `(parent, segment) → child id`. A slot packs `(child id + 1) << 32 |
/// hash tag` in one atomic word — zero means empty. Readers probe lock-free; the single writer (under
/// the tree's writer lock) installs entries with a release store and grows by publishing a rehashed
/// copy through the tree's `current` pointer (retired tables stay alive, so a reader holding the old
/// pointer sees a valid — merely stale — subset and falls into the writer path on a miss).
struct Table {
    mask: usize,
    slots: Box<[AtomicU64]>,
}

impl Table {
    fn new(cap: usize) -> Self {
        debug_assert!(cap.is_power_of_two());
        Table {
            mask: cap - 1,
            slots: (0..cap).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    #[inline]
    fn probe(&self, arena: &Arena, parent: NameId, segment: &str, h: u64) -> Option<NameId> {
        let tag = h as u32;
        let mut i = (h >> 32) as usize & self.mask;
        loop {
            let slot = self.slots[i].load(Ordering::Acquire);
            if slot == 0 {
                return None;
            }
            if slot as u32 == tag {
                let id = ((slot >> 32) - 1) as u32;
                let node = arena.get(id);
                if node.parent == Some(parent) && &*node.segment == segment {
                    return Some(NameId(id));
                }
            }
            i = (i + 1) & self.mask;
        }
    }

    /// Writer-only: install `child` at the first empty slot of its probe chain.
    fn install(&self, h: u64, child: u32) {
        let value = (u64::from(child) + 1) << 32 | u64::from(h as u32);
        let mut i = (h >> 32) as usize & self.mask;
        loop {
            if self.slots[i].load(Ordering::Relaxed) == 0 {
                self.slots[i].store(value, Ordering::Release);
                return;
            }
            i = (i + 1) & self.mask;
        }
    }
}

#[inline]
fn child_hash(parent: NameId, segment: &str) -> u64 {
    use std::hash::Hasher;
    let mut h = FxHasher::default();
    h.write_u32(parent.0);
    h.write(segment.as_bytes());
    h.finish()
}

struct WriterState {
    /// Every table ever published, oldest first; the last is the live one. Retired tables are kept so
    /// readers holding a stale `current` pointer stay valid. The boxes are required, not an
    /// indirection nicety: `current` aliases a table by raw pointer, so a table must never move when
    /// this vector reallocates.
    #[allow(clippy::vec_box)]
    tables: Vec<Box<Table>>,
    /// Number of installed children (across live entries; retirement never removes entries).
    count: usize,
}

/// A compact internal-name tree. Names are inserted as slash-separated segments and retained structures
/// store `NameId` (`u32`) handles instead of cloning full internal-name strings. All reads — id walks
/// (`render`, `starts_with`, …) and child probes (`get`, `existing_child_of`) — are lock-free; only a
/// genuinely new insertion takes the writer lock.
pub struct NameTree {
    arena: Arena,
    current: AtomicPtr<Table>,
    writer: Mutex<WriterState>,
}

// SAFETY: readers only follow published data (acquire loads of table slots / the table pointer pairing
// with the writer's release stores); all mutation is serialized by the writer lock.
unsafe impl Sync for NameTree {}
unsafe impl Send for NameTree {}

impl Default for NameTree {
    fn default() -> Self {
        let arena = Arena::new();
        arena.push(NameNode {
            parent: None,
            sep: 0,
            segment: std::sync::Arc::from(""),
        });
        let table = Box::new(Table::new(BASE as usize));
        let current = AtomicPtr::new(&*table as *const Table as *mut Table);
        NameTree {
            arena,
            current,
            writer: Mutex::new(WriterState {
                tables: vec![table],
                count: 0,
            }),
        }
    }
}

impl Clone for NameTree {
    fn clone(&self) -> Self {
        // Nodes are inserted parents-first, so replaying them in id order reproduces identical ids.
        let out = NameTree::default();
        for id in 1..self.arena.len() {
            let node = self.arena.get(id);
            let parent = node.parent.expect("non-root name node has a parent");
            out.child_or_insert(parent, node.sep, &node.segment);
        }
        out
    }
}

impl std::fmt::Debug for NameTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameTree")
            .field("len", &self.arena.len())
            .finish_non_exhaustive()
    }
}

impl NameTree {
    pub const ROOT: NameId = NameId(0);

    pub fn insert(&self, internal: &str) -> NameId {
        if internal.is_empty() {
            return Self::ROOT;
        }
        let mut parent = Self::ROOT;
        let mut sep = 0;
        for segment in internal.split('/') {
            parent = self.child_or_insert(parent, sep, segment);
            sep = b'/';
        }
        parent
    }

    pub fn get(&self, internal: &str) -> Option<NameId> {
        if internal.is_empty() {
            return Some(Self::ROOT);
        }
        let mut parent = Self::ROOT;
        for segment in internal.split('/') {
            parent = self.child(parent, segment)?;
        }
        Some(parent)
    }

    /// One child step below `parent`; `segment` must not contain `/`.
    pub fn child_of(&self, parent: NameId, segment: &str) -> NameId {
        let sep = if parent == Self::ROOT { 0 } else { b'/' };
        self.child_or_insert(parent, sep, segment)
    }

    /// The already-interned child of `parent` for `segment`, without inserting.
    pub fn existing_child_of(&self, parent: NameId, segment: &str) -> Option<NameId> {
        self.child(parent, segment)
    }

    pub fn insert_from(&self, other: &NameTree, id: NameId) -> NameId {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != Self::ROOT {
            let node = other.node(cur);
            parts.push((node.sep, &*node.segment));
            cur = node.parent.expect("non-root name node has a parent");
        }
        let mut parent = Self::ROOT;
        for (sep, segment) in parts.into_iter().rev() {
            parent = self.child_or_insert(parent, sep, segment);
        }
        parent
    }

    pub fn render(&self, id: NameId) -> String {
        if id == Self::ROOT {
            return String::new();
        }
        let mut parts = Vec::new();
        let mut len = 0usize;
        let mut cur = id;
        while cur != Self::ROOT {
            let node = self.node(cur);
            len += node.segment.len() + usize::from(node.sep != 0);
            parts.push((node.sep, &*node.segment));
            cur = node.parent.expect("non-root name node has a parent");
        }
        let mut out = String::with_capacity(len);
        for (sep, segment) in parts.into_iter().rev() {
            if sep != 0 {
                out.push(sep as char);
            }
            out.push_str(segment);
        }
        out
    }

    pub fn starts_with(&self, id: NameId, prefix: &str) -> bool {
        if prefix.is_empty() {
            return true;
        }
        let mut matched = 0usize;
        for b in self.path_bytes(id) {
            if matched == prefix.len() {
                return true;
            }
            if prefix.as_bytes()[matched] != b {
                return false;
            }
            matched += 1;
        }
        matched == prefix.len()
    }

    pub fn strip_prefix(&self, id: NameId, prefix: &str) -> Option<String> {
        let prefix = prefix.as_bytes();
        let mut matched = 0usize;
        let mut suffix = Vec::new();
        for b in self.path_bytes(id) {
            if matched < prefix.len() {
                if prefix[matched] != b {
                    return None;
                }
                matched += 1;
            } else {
                suffix.push(b);
            }
        }
        (matched == prefix.len()).then(|| {
            String::from_utf8(suffix).expect("name-tree segments are inserted from UTF-8 strings")
        })
    }

    pub fn unsigned_suffix_after_prefix(&self, id: NameId, prefix: &str) -> Option<usize> {
        let prefix = prefix.as_bytes();
        let mut matched = 0usize;
        let mut value = None;
        for b in self.path_bytes(id) {
            if matched < prefix.len() {
                if prefix[matched] != b {
                    return None;
                }
                matched += 1;
            } else if b.is_ascii_digit() {
                let digit = usize::from(b - b'0');
                value = Some(
                    value
                        .unwrap_or(0usize)
                        .checked_mul(10)?
                        .checked_add(digit)?,
                );
            } else {
                return None;
            }
        }
        (matched == prefix.len()).then_some(value).flatten()
    }

    pub fn contains(&self, id: NameId, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let n = needle.as_bytes();
        let mut pi = vec![0usize; n.len()];
        for i in 1..n.len() {
            let mut j = pi[i - 1];
            while j > 0 && n[i] != n[j] {
                j = pi[j - 1];
            }
            if n[i] == n[j] {
                j += 1;
            }
            pi[i] = j;
        }
        let mut j = 0usize;
        for b in self.path_bytes(id) {
            while j > 0 && b != n[j] {
                j = pi[j - 1];
            }
            if b == n[j] {
                j += 1;
            }
            if j == n.len() {
                return true;
            }
        }
        false
    }

    pub fn qualifier_matches(&self, id: NameId, qualifier: &str) -> bool {
        self.get(qualifier) == Some(id) || &*self.node(id).segment == qualifier
    }

    pub fn package_matches(&self, id: NameId, package: &str) -> bool {
        let Some(parent) = self.parent(id) else {
            return package.is_empty();
        };
        self.path_eq(parent, package)
    }

    pub fn package(&self, id: NameId) -> String {
        self.parent(id)
            .map_or_else(String::new, |parent| self.render(parent))
    }

    pub fn nested_separator_matches(&self, left: NameId, right: NameId) -> bool {
        let (left_len, left_nested_start) = self.path_len_and_nested_start(left);
        let (right_len, right_nested_start) = self.path_len_and_nested_start(right);
        if left_len != right_len || left_nested_start != right_nested_start {
            return false;
        }
        self.path_bytes(left)
            .zip(self.path_bytes(right))
            .enumerate()
            .all(|(idx, (a, b))| {
                a == b || (idx >= left_nested_start && matches!((a, b), (b'.' | b'$', b'.' | b'$')))
            })
    }

    pub fn parent(&self, id: NameId) -> Option<NameId> {
        self.node(id).parent
    }

    pub fn segment(&self, id: NameId) -> &str {
        &self.node(id).segment
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.arena.len() as usize
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.arena.len() == 0
    }

    fn node(&self, id: NameId) -> &NameNode {
        self.arena.get(id.0)
    }

    fn child_or_insert(&self, parent: NameId, sep: u8, segment: &str) -> NameId {
        let h = child_hash(parent, segment);
        // Lock-free fast path: probe the live table.
        // SAFETY: `current` always points at a table owned by `writer.tables`, which never drops one
        // while the tree is alive.
        let table = unsafe { &*self.current.load(Ordering::Acquire) };
        if let Some(id) = table.probe(&self.arena, parent, segment, h) {
            return id;
        }
        let mut w = self.writer.lock().unwrap();
        // Re-probe the (possibly newer) live table: another writer may have inserted this child, or
        // grown the table, between the fast-path probe and taking the lock.
        let mut table = unsafe { &*self.current.load(Ordering::Relaxed) };
        if let Some(id) = table.probe(&self.arena, parent, segment, h) {
            return id;
        }
        // Grow at 7/8 load, BEFORE installing, so the probe chains stay short and an empty slot always
        // exists. The rehashed copy is fully built and then published; retired tables stay alive for
        // readers still holding the old pointer.
        if (w.count + 1) * 8 > (table.mask + 1) * 7 {
            let grown = Box::new(Table::new((table.mask + 1) * 2));
            for slot in table.slots.iter() {
                let s = slot.load(Ordering::Relaxed);
                if s != 0 {
                    let id = ((s >> 32) - 1) as u32;
                    let node = self.arena.get(id);
                    let parent = node.parent.expect("child entries have a parent");
                    grown.install(child_hash(parent, &node.segment), id);
                }
            }
            self.current
                .store(&*grown as *const Table as *mut Table, Ordering::Release);
            w.tables.push(grown);
            table = unsafe { &*self.current.load(Ordering::Relaxed) };
        }
        let segment: std::sync::Arc<str> = std::sync::Arc::from(segment);
        let id = self.arena.push(NameNode {
            parent: Some(parent),
            sep,
            segment,
        });
        table.install(h, id);
        w.count += 1;
        NameId(id)
    }

    fn child(&self, parent: NameId, segment: &str) -> Option<NameId> {
        let h = child_hash(parent, segment);
        // SAFETY: see `child_or_insert`.
        let table = unsafe { &*self.current.load(Ordering::Acquire) };
        table.probe(&self.arena, parent, segment, h)
    }

    fn path_eq(&self, id: NameId, path: &str) -> bool {
        let mut matched = 0usize;
        for b in self.path_bytes(id) {
            if matched == path.len() || path.as_bytes()[matched] != b {
                return false;
            }
            matched += 1;
        }
        matched == path.len()
    }

    fn path_len_and_nested_start(&self, id: NameId) -> (usize, usize) {
        let mut len = 0usize;
        let mut nested_start = 0usize;
        for b in self.path_bytes(id) {
            len += 1;
            if b == b'/' {
                nested_start = len;
            }
        }
        (len, nested_start)
    }

    fn path_bytes(&self, id: NameId) -> impl Iterator<Item = u8> + '_ {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != Self::ROOT {
            let node = self.node(cur);
            parts.push((node.sep, node.segment.as_bytes()));
            cur = node.parent.expect("non-root name node has a parent");
        }
        parts
            .into_iter()
            .rev()
            .flat_map(|(sep, segment)| std::iter::once(sep).chain(segment.iter().copied()))
            .filter(|b| *b != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::NameTree;

    #[test]
    fn compares_paths_without_rendering() {
        let names = NameTree::default();
        let map = names.insert("kotlin/collections/Map");
        let entry = names.insert("kotlin/collections/Map$Entry");

        assert!(names.starts_with(map, "kotlin/collections/"));
        assert!(names.starts_with(map, "kotlin/collections/Ma"));
        assert_eq!(
            names.strip_prefix(map, "kotlin/collections/"),
            Some("Map".to_string())
        );
        assert_eq!(names.strip_prefix(map, "kotlin/List"), None);
        let function = names.insert("kotlin/jvm/functions/Function12");
        assert_eq!(
            names.unsigned_suffix_after_prefix(function, "kotlin/jvm/functions/Function"),
            Some(12)
        );
        assert_eq!(
            names.unsigned_suffix_after_prefix(function, "kotlin/jvm/functions/Function12"),
            None
        );
        assert_eq!(
            names.unsigned_suffix_after_prefix(map, "kotlin/jvm/functions/Function"),
            None
        );
        assert!(names.contains(map, "collections/Map"));
        assert!(names.qualifier_matches(map, "Map"));
        assert!(names.qualifier_matches(map, "kotlin/collections/Map"));
        assert!(!names.qualifier_matches(map, "Entry"));
        assert!(names.package_matches(map, "kotlin/collections"));
        assert!(!names.package_matches(entry, "kotlin"));
        assert_eq!(names.package(entry), "kotlin/collections");
    }

    #[test]
    fn grows_past_the_initial_table_and_chunks() {
        let names = NameTree::default();
        let ids: Vec<_> = (0..3000)
            .map(|i| names.insert(&format!("pkg{}/Class{}", i % 7, i)))
            .collect();
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(names.render(*id), format!("pkg{}/Class{}", i % 7, i));
        }
        let again = names.insert("pkg3/Class10");
        assert_eq!(ids[10], again);
    }

    #[test]
    fn clone_replays_identical_ids_and_isolates_growth() {
        let names = NameTree::default();
        let map = names.insert("kotlin/collections/Map");
        let entry = names.insert("kotlin/collections/Map$Entry");
        // Past a table growth, so the clone replays a grown tree.
        let ids: Vec<_> = (0..200).map(|i| names.insert(&format!("p/C{i}"))).collect();

        let copy = names.clone();
        assert_eq!(copy.len(), names.len());
        assert_eq!(copy.get("kotlin/collections/Map"), Some(map));
        assert_eq!(copy.render(entry), "kotlin/collections/Map$Entry");
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(copy.render(*id), format!("p/C{i}"));
        }
        // The copy grows independently of the original.
        let only_in_copy = copy.insert("copy/Only");
        assert_eq!(copy.render(only_in_copy), "copy/Only");
        assert_eq!(names.get("copy/Only"), None);
    }

    #[test]
    fn insert_from_maps_ids_across_trees() {
        let src = NameTree::default();
        let id = src.insert("a/b/C");
        let dst = NameTree::default();
        dst.insert("unrelated/Name");
        let moved = dst.insert_from(&src, id);
        assert_eq!(dst.render(moved), "a/b/C");
        assert_eq!(dst.insert_from(&src, NameTree::ROOT), NameTree::ROOT);
        // Re-inserting the same source id is idempotent in the destination.
        assert_eq!(dst.insert_from(&src, id), moved);
    }

    #[test]
    fn debug_reports_len_without_walking() {
        let names = NameTree::default();
        names.insert("a/b");
        let dbg = format!("{names:?}");
        assert!(dbg.contains("NameTree"), "{dbg}");
        assert!(dbg.contains("len"), "{dbg}");
    }

    #[test]
    fn concurrent_insert_and_read() {
        let names = std::sync::Arc::new(NameTree::default());
        let mut handles = Vec::new();
        for t in 0..8 {
            let names = names.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..2000 {
                    // Half the names are shared across threads, half unique — exercises racing
                    // insert-vs-probe on the same (parent, segment) pairs and table growth.
                    let name = if i % 2 == 0 {
                        format!("shared/pkg{}/Class{}", i % 5, i)
                    } else {
                        format!("thread{t}/pkg/Class{i}")
                    };
                    let id = names.insert(&name);
                    assert_eq!(names.render(id), name);
                    assert_eq!(names.get(&name), Some(id));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }
}
