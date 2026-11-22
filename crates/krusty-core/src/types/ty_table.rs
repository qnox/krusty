//! The process-wide table that gives every distinct type one 4-byte [`Ty`] id.
//!
//! Types are hash-consed: [`Ty::intern`] returns the existing id of an equal [`TyKind`], so equal
//! types share an id and `Ty` equality and hashing compare ids. The table only grows. A kind is
//! written into its slot before its id is handed out, so [`Ty::kind`] reads a slot without
//! taking a lock.
//!
//! The types with an associated constant (`Ty::Unit`, `Ty::Int`, …) have reserved ids below
//! [`RESERVED`]; their kinds come from a constant array and never touch the table.

use super::{Ty, TyKind};
use crate::fx_hash::{FxBuildHasher, FxHasher};
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{OnceLock, RwLock};

/// The kinds of the reserved ids, in id order. `Ty`'s associated constants name these ids.
const RESERVED_KINDS: [TyKind; 18] = [
    TyKind::Unit,
    TyKind::Null,
    TyKind::Nothing,
    TyKind::Error,
    TyKind::Pending,
    TyKind::BOOLEAN,
    TyKind::BYTE,
    TyKind::SHORT,
    TyKind::INT,
    TyKind::LONG,
    TyKind::CHAR,
    TyKind::FLOAT,
    TyKind::DOUBLE,
    TyKind::UBYTE,
    TyKind::USHORT,
    TyKind::UINT,
    TyKind::ULONG,
    TyKind::STRING,
];
const RESERVED: u32 = RESERVED_KINDS.len() as u32;

/// Slots in the first segment. Segment `n` holds `FIRST_SEGMENT << n` slots, so a table that
/// grows never moves a slot and the small common case allocates little.
const FIRST_SEGMENT_BITS: u32 = 12;
const SEGMENTS: usize = (u32::BITS - FIRST_SEGMENT_BITS + 1) as usize;

const SHARDS: usize = 64;

type Segment = Box<[OnceLock<TyKind>]>;

struct TyTable {
    ids: [RwLock<HashMap<TyKind, u32, FxBuildHasher>>; SHARDS],
    next: AtomicU32,
}

static SLOTS: [OnceLock<Segment>; SEGMENTS] = [const { OnceLock::new() }; SEGMENTS];

fn table() -> &'static TyTable {
    static TABLE: OnceLock<TyTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let ids: [RwLock<HashMap<TyKind, u32, FxBuildHasher>>; SHARDS] =
            std::array::from_fn(|_| RwLock::new(HashMap::default()));
        for (id, kind) in (0..RESERVED).zip(RESERVED_KINDS) {
            ids[shard(&kind)].write().unwrap().insert(kind, id);
        }
        TyTable {
            ids,
            next: AtomicU32::new(RESERVED),
        }
    })
}

fn kind_hash(kind: &TyKind) -> u64 {
    let mut hasher = FxHasher::default();
    kind.hash(&mut hasher);
    hasher.finish()
}

fn shard(kind: &TyKind) -> usize {
    kind_hash(kind) as usize % SHARDS
}

/// Entries in each thread's table of recently interned kinds.
const RECENT_BITS: u32 = 12;

/// One recently interned kind and its id per entry, indexed by the top bits of the kind's hash.
type RecentKinds = Box<[Cell<Option<(TyKind, u32)>>]>;

thread_local! {
    /// A direct-mapped table of kinds this thread interned recently. Most constructions repeat a
    /// kind seen moments before; finding it here skips the shard lock. An entry is only ever a
    /// published id, so a stale entry is still correct.
    static RECENT: RecentKinds =
        (0..1usize << RECENT_BITS).map(|_| Cell::new(None)).collect();
}

/// The segment and offset of `id`'s slot.
#[inline]
fn slot_position(id: u32) -> (usize, usize) {
    let biased = u64::from(id) + (1 << FIRST_SEGMENT_BITS);
    let top = u64::BITS - 1 - biased.leading_zeros();
    let segment = (top - FIRST_SEGMENT_BITS) as usize;
    (segment, (biased - (1 << top)) as usize)
}

fn slot(id: u32) -> &'static OnceLock<TyKind> {
    let (segment, offset) = slot_position(id);
    let slots = SLOTS[segment].get_or_init(|| {
        (0..(1usize << (FIRST_SEGMENT_BITS as usize + segment)))
            .map(|_| OnceLock::new())
            .collect()
    });
    &slots[offset]
}

impl Ty {
    /// The id of `kind`, interning it on first sight.
    pub(super) fn intern(kind: TyKind) -> Ty {
        let hash = kind_hash(&kind);
        let recent = (hash >> (u64::BITS - RECENT_BITS)) as usize;
        if let Ok(Some((seen, id))) = RECENT.try_with(|entries| entries[recent].get()) {
            if seen == kind {
                return Ty(id);
            }
        }
        let id = Ty::intern_shared(kind, hash as usize % SHARDS);
        let _ = RECENT.try_with(|entries| entries[recent].set(Some((kind, id.0))));
        id
    }

    fn intern_shared(kind: TyKind, shard: usize) -> Ty {
        let table = table();
        let ids = &table.ids[shard];
        if let Some(&id) = ids.read().unwrap().get(&kind) {
            return Ty(id);
        }
        let mut ids = ids.write().unwrap();
        if let Some(&id) = ids.get(&kind) {
            return Ty(id);
        }
        let id = table.next.fetch_add(1, Ordering::Relaxed);
        assert!(id != u32::MAX, "the type table is full");
        slot(id).set(kind).expect("a type id is published once");
        ids.insert(kind, id);
        Ty(id)
    }

    /// This type's outermost layer, for matching.
    #[inline]
    pub fn kind(self) -> TyKind {
        if self.0 < RESERVED {
            return RESERVED_KINDS[self.0 as usize];
        }
        let (segment, offset) = slot_position(self.0);
        *SLOTS[segment]
            .get()
            .and_then(|slots| slots[offset].get())
            .expect("type ids come from Ty::intern")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_positions_tile_the_id_space() {
        assert_eq!(slot_position(0), (0, 0));
        assert_eq!(slot_position((1 << FIRST_SEGMENT_BITS) - 1), (0, 4095));
        assert_eq!(slot_position(1 << FIRST_SEGMENT_BITS), (1, 0));
        assert_eq!(slot_position(3 << FIRST_SEGMENT_BITS), (2, 0));
        assert_eq!(slot_position(u32::MAX - 1).0, SEGMENTS - 1);
    }

    #[test]
    fn interning_a_reserved_kind_returns_its_reserved_id() {
        for (id, kind) in (0..RESERVED).zip(RESERVED_KINDS) {
            assert_eq!(Ty::intern(kind), Ty(id));
            assert_eq!(Ty(id).kind(), kind);
        }
    }

    #[test]
    fn concurrent_equal_kinds_share_one_id() {
        let kind = TyKind::Nullable(Ty::intern(TyKind::DefinitelyNotNull(Ty::Long)));
        let ids = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| scope.spawn(move || Ty::intern(kind)))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(ids[0].kind(), kind);
    }

    /// Ids allocated into segments that did not exist yet are readable from another thread, and
    /// the ids that thread already holds keep their kinds while new segments are published.
    #[test]
    fn new_segments_publish_while_another_thread_reads_earlier_ids() {
        let root = Ty::intern(TyKind::Obj(
            super::super::type_name("ty_table_test/SegmentRoot"),
            super::super::TyList::EMPTY,
        ));
        let (sender, receiver) = std::sync::mpsc::channel::<(Ty, TyKind)>();
        std::thread::scope(|scope| {
            let reader = scope.spawn(move || {
                let mut held = Vec::new();
                for (ty, kind) in receiver {
                    assert_eq!(ty.kind(), kind);
                    held.push((ty, kind));
                    if held.len() % 512 == 0 {
                        assert!(held.iter().all(|&(ty, kind)| ty.kind() == kind));
                    }
                }
                assert!(held.iter().all(|&(ty, kind)| ty.kind() == kind));
                held.len()
            });
            // Each kind wraps the previous type, so every one is new to the table.
            let mut previous = root;
            let first_segment = slot_position(root.0).0;
            let mut sent = 0;
            while slot_position(previous.0).0 < first_segment + 2 {
                let kind = TyKind::OutProjection(previous);
                previous = Ty::intern(kind);
                sender.send((previous, kind)).unwrap();
                sent += 1;
            }
            drop(sender);
            assert_eq!(reader.join().unwrap(), sent);
        });
    }
}
