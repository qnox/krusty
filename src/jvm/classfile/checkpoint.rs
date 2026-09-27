//! Rolling a class back to an earlier point of its writing: the constants and source-map lines of
//! code compiled only to be read back as a method node and written into another class.

use super::{ClassWriter, ConstPool};
use crate::jvm::source_map::SourceMap;

/// A point in a class's writing that [`ClassWriter::rollback`] returns its pool and source map to.
pub struct ClassCheckpoint {
    entries: usize,
    slots: usize,
    declarations: [usize; 5],
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
            declarations: self.declaration_counts(),
            source_map: self.source_map.clone(),
        }
    }

    /// Forget the constants and source-map lines compiled since `checkpoint`: code compiled only to
    /// be read back as a method node and written elsewhere, which kotlinc never writes into this
    /// class. Nothing is forgotten, and `false` returned, when that code also declared something in
    /// the class (a method, field, bootstrap method or nested class), which would still name them.
    pub fn rollback(&mut self, checkpoint: ClassCheckpoint) -> bool {
        if self.declaration_counts() != checkpoint.declarations {
            return false;
        }
        self.cp.truncate(checkpoint.entries, checkpoint.slots);
        self.source_map = checkpoint.source_map;
        true
    }

    fn declaration_counts(&self) -> [usize; 5] {
        [
            self.fields.len(),
            self.late_fields.len(),
            self.methods.len(),
            self.bootstrap_methods.len(),
            self.inner_class_candidates.len(),
        ]
    }
}
