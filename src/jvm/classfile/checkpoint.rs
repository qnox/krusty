//! Rolling a class back to an earlier point of its writing: the constants and source-map lines of
//! code compiled only to be read back as a method node and written into another class.

use super::{ClassWriter, ConstPool};
use crate::jvm::source_map::SourceMap;

/// A point in a class's writing that [`ClassWriter::rollback`] returns its pool and source map to.
pub struct ClassCheckpoint {
    entries: usize,
    slots: usize,
    members: [usize; 3],
    bootstrap_methods: usize,
    inner_class_candidates: usize,
    source_map: SourceMap,
}

impl ConstPool {
    /// Forget every entry interned past the first `entries` (whose slots start past `slots`).
    fn truncate(&mut self, entries: usize, slots: usize) {
        for forgotten in self.entries.drain(entries..) {
            self.dedup.remove(&forgotten);
        }
        self.slot_entries.truncate(slots);
    }
}

impl ClassWriter {
    /// What code compiled from here on may leave behind in the class, for [`Self::rollback`].
    pub fn checkpoint(&self) -> ClassCheckpoint {
        ClassCheckpoint {
            entries: self.cp.entries.len(),
            slots: self.cp.slot_entries.len(),
            members: self.member_counts(),
            bootstrap_methods: self.bootstrap_methods.len(),
            inner_class_candidates: self.inner_class_candidates.len(),
            source_map: self.source_map.clone(),
        }
    }

    /// Forget what code compiled since `checkpoint` left in the class: its constants, source-map
    /// lines, bootstrap methods and nested-class candidates, all of which only that code referred
    /// to. It was compiled to be read back as a method node and written elsewhere, which kotlinc
    /// never writes into this class. Nothing is forgotten, and `false` returned, when that code
    /// also declared a member (a method or field) of the class, which the class would still carry.
    pub fn rollback(&mut self, checkpoint: ClassCheckpoint) -> bool {
        if self.member_counts() != checkpoint.members {
            return false;
        }
        self.cp.truncate(checkpoint.entries, checkpoint.slots);
        self.bootstrap_methods
            .truncate(checkpoint.bootstrap_methods);
        self.inner_class_candidates
            .truncate(checkpoint.inner_class_candidates);
        self.source_map = checkpoint.source_map;
        true
    }

    fn member_counts(&self) -> [usize; 3] {
        [
            self.fields.len(),
            self.late_fields.len(),
            self.methods.len(),
        ]
    }
}
