//! Compact physical locations for classes stored in a JDK jimage.

use crate::name_tree::{NameId, NameTree};

/// High bit of a location's packed size. On-disk class sizes fit in the low 31 bits.
const JIMAGE_COMPRESSED: u32 = 1 << 31;

/// One class resource in the jimage, indexed by [`NameId`].
///
/// A hash map of ~30k classes keeps a power-of-two slot table (64k entries, key plus a fat
/// `(offset, size, flag)` value). Name ids are arena-sequential, and a real content offset is never
/// zero — jimage content begins after the header and tables — so an absent name is `offset == 0`.
/// Offset and on-disk size each fit in 32 bits for a JDK `lib/modules` (well under 4 GiB; class
/// resources are far under 2 GiB), which packs a slot into 8 bytes.
#[derive(Debug, Default)]
pub(super) struct JimageLocations {
    slots: Vec<JimageSlot>,
    classes: u32,
}

#[derive(Clone, Copy, Debug)]
struct JimageSlot {
    offset: u32,
    packed: u32,
}

impl JimageLocations {
    pub(super) fn reserve_classes(&mut self, table_length: usize) {
        // Package nodes take extra ids, so the vector still grows past the class count. Reserving
        // the redirect table length skips the first several doublings.
        self.slots.reserve(table_length);
    }

    /// First location wins, matching `HashMap::entry().or_insert`.
    pub(super) fn insert(&mut self, id: NameId, offset: u64, size: usize, compressed: bool) {
        // A JDK `lib/modules` content offset is a few hundred megabytes. Above 4 GiB the slot
        // cannot represent it, so the class stays absent rather than truncating onto another offset.
        let Ok(offset) = u32::try_from(offset) else {
            return;
        };
        if offset == 0 {
            return;
        }
        let index = id.index() as usize;
        if index >= self.slots.len() {
            self.slots.resize(
                index + 1,
                JimageSlot {
                    offset: 0,
                    packed: 0,
                },
            );
        }
        if self.slots[index].offset != 0 {
            return;
        }
        // The top bit stores the compression flag. An unrepresentable resource is not a valid
        // packed location; keeping it absent avoids seek-reading a truncated byte range.
        let Ok(size) = u32::try_from(size) else {
            return;
        };
        if size & JIMAGE_COMPRESSED != 0 {
            return;
        }
        self.slots[index] = JimageSlot {
            offset,
            packed: if compressed {
                size | JIMAGE_COMPRESSED
            } else {
                size
            },
        };
        self.classes += 1;
    }

    pub(super) fn get(&self, id: NameId) -> Option<(u64, usize, bool)> {
        let slot = self.slots.get(id.index() as usize)?;
        if slot.offset == 0 {
            return None;
        }
        let compressed = slot.packed & JIMAGE_COMPRESSED != 0;
        let size = (slot.packed & !JIMAGE_COMPRESSED) as usize;
        Some((u64::from(slot.offset), size, compressed))
    }

    /// The classes with a recorded location, as ids the owning `names` tree has published.
    pub(super) fn class_ids<'a>(
        &'a self,
        names: &'a NameTree,
    ) -> impl Iterator<Item = NameId> + 'a {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.offset != 0)
            .filter_map(|(index, _)| names.id_at(index as u32))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.classes == 0
    }

    pub(super) fn class_count(&self) -> usize {
        self.classes as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_pack_size_and_compressed_flag() {
        let names = NameTree::default();
        let ids: Vec<NameId> = (0..9).map(|n| names.insert(&format!("p/C{n}"))).collect();
        let (first, string, object) = (ids[0], ids[3], ids[8]);
        let mut locations = JimageLocations::default();
        locations.insert(string, 1, 2, false);
        locations.insert(string, 99, 7, true);
        locations.insert(object, 40, 50, true);

        assert_eq!(locations.get(string), Some((1, 2, false)));
        assert_eq!(locations.get(object), Some((40, 50, true)));
        assert_eq!(locations.get(first), None);
        assert_eq!(locations.class_count(), 2);
        assert_eq!(
            locations.class_ids(&names).collect::<Vec<_>>(),
            vec![string, object]
        );

        locations.insert(ids[4], 0, 1, false);
        assert_eq!(locations.class_count(), 2);
        assert_eq!(locations.get(ids[4]), None);

        locations.insert(ids[5], 50, JIMAGE_COMPRESSED as usize, false);
        assert_eq!(locations.class_count(), 2);
        assert_eq!(locations.get(ids[5]), None);
    }

    #[test]
    fn class_ids_skip_slots_the_tree_has_not_published() {
        let names = NameTree::default();
        let published = names.insert("p/A");
        let mut locations = JimageLocations::default();
        locations.insert(published, 1, 2, false);
        let foreign = NameTree::default();
        let beyond = (0..50)
            .map(|n| foreign.insert(&format!("q/B{n}")))
            .last()
            .unwrap();
        locations.insert(beyond, 1, 2, false);

        assert_eq!(
            locations.class_ids(&names).collect::<Vec<_>>(),
            vec![published]
        );
    }
}
