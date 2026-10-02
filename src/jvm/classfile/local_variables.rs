//! A method's `LocalVariableTable` strings: when its names and descriptors intern.

use super::{ClassWriter, CodeBuilder};

impl CodeBuilder {
    /// How many `LocalVariableTable` entries the method has recorded so far.
    pub(crate) fn local_entry_count(&self) -> usize {
        self.local_entries.len()
    }

    /// Record a local's entry at `position` in table order, ahead of the entries recorded since
    /// that position was read. kotlinc's inliner writes an inlined lambda's inline-depth marker
    /// before the locals the lambda's body declares, although its range closes after theirs.
    pub(crate) fn insert_local_entry(
        &mut self,
        position: usize,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let position = position.min(self.local_entries.len());
        self.local_entries.insert(position, entry);
    }
}

impl ClassWriter {
    /// Intern a method's `LocalVariableTable` names and descriptors, in table order, once its body
    /// is complete. ASM visits the local variables after the instructions and before `visitMaxs`,
    /// so a nested block's local follows every constant of the method body, not the instruction
    /// that closed its scope, and precedes the frame classes.
    pub(super) fn intern_local_table(&mut self, code: &CodeBuilder) {
        for (_, _, _, local, descriptor) in code.local_entries() {
            self.cp.utf8(local);
            self.cp.utf8(descriptor);
        }
    }

    /// Intern a method's `LocalVariableTable` names and descriptors BEFORE its code is added.
    ///
    /// ASM visits `visitLocalVariable` before `visitMaxs`, so kotlinc's pool carries a local's name
    /// and descriptor ahead of every class constant the frame computation introduces. krusty builds
    /// the `StackMapTable` inside `add_method`, which interns each parameter's verification type —
    /// so without this reservation a parameter whose class appears NOWHERE else in the class file
    /// (the serialization constructor's marker) lands ahead of the local names instead of behind
    /// them.
    pub fn reserve_method_lvt(&mut self, locals: &[(String, String, u16)]) {
        for (name, descriptor, _) in locals {
            self.cp.utf8(name);
            self.cp.utf8(descriptor);
        }
    }
}
