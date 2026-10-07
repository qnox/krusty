//! A method's `LocalVariableTable` strings: when its names and descriptors intern.

use super::{ClassWriter, CodeBuilder, LocalEntryPlacement, LvtEntry};

impl CodeBuilder {
    /// Record a local owned by an inline frame. Its marker may close later and insert immediately
    /// ahead of the frame's first already-recorded local.
    pub(crate) fn add_inline_frame_local(
        &mut self,
        frame: u64,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let position = self
            .local_entry_placements
            .iter()
            .position(|placement| {
                matches!(placement, LocalEntryPlacement::FrameMarker(owner) if *owner == frame)
            })
            .and_then(|marker| {
                self.local_entry_placements[marker + 1..]
                    .iter()
                    .position(|placement| matches!(placement, LocalEntryPlacement::FrameMarker(_)))
                    .map(|next| marker + 1 + next)
            })
            .unwrap_or(self.local_entries.len());
        self.local_entries.insert(position, entry);
        self.local_entry_placements
            .insert(position, LocalEntryPlacement::FrameLocal(frame));
    }

    /// Record an inline frame marker before its own already-recorded locals, while retaining every
    /// nested-frame and catch row that precedes them.
    pub(crate) fn insert_inline_frame_marker(
        &mut self,
        frame: u64,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let first_local = self
            .local_entry_placements
            .iter()
            .position(|placement| {
                matches!(placement, LocalEntryPlacement::FrameLocal(owner) if *owner == frame)
            });
        let position = first_local.unwrap_or_else(|| {
            self.local_entry_placements
                .iter()
                .rposition(|placement| {
                    matches!(
                        placement,
                        LocalEntryPlacement::FrameBeforeMarker(owner) if *owner == frame
                    )
                })
                .map_or(self.local_entries.len(), |position| position + 1)
        });
        self.local_entries.insert(position, entry);
        self.local_entry_placements
            .insert(position, LocalEntryPlacement::FrameMarker(frame));
    }

    /// Record a catch row before its containing inline frame's marker. Handler emission may close
    /// after that marker's lexical scope; placement must not depend on that timing.
    pub(crate) fn add_inline_frame_catch(
        &mut self,
        frame: u64,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let position = self
            .local_entry_placements
            .iter()
            .position(|placement| {
                matches!(placement, LocalEntryPlacement::FrameMarker(owner) if *owner == frame)
            })
            .unwrap_or(self.local_entries.len());
        self.local_entries.insert(position, entry);
        self.local_entry_placements
            .insert(position, LocalEntryPlacement::FrameBeforeMarker(frame));
    }
}

impl ClassWriter {
    /// The `LocalVariableTable` a method's emission recorded, its strings interned.
    pub(super) fn local_table(&mut self, code: &CodeBuilder) -> Vec<LvtEntry> {
        code.local_entries()
            .iter()
            // A `LocalVariableTable` `start_pc` must index the code array (JVMS §4.7.13) —
            // HotSpot's class-file parser rejects the whole class otherwise. A local DECLARED
            // in a region the emitter dropped as unreachable (`val y: Int = boom() ?: 1`) has
            // its start recorded past the last instruction and describes no live range, so it
            // goes with the code. An empty range stays until the method is written, as in
            // kotlinc (`prepareForEmitting`): the store before it is a named local's.
            .filter(|(start, len, ..)| {
                let start = usize::from(*start);
                let end = len.map_or(code.bytes.len(), |length| start + usize::from(length));
                start < code.bytes.len() && end <= code.bytes.len()
            })
            .map(|(start, len, slot, name, descriptor)| {
                (
                    self.cp.utf8(name),
                    self.cp.utf8(descriptor),
                    *slot,
                    Some(*start),
                    *len,
                )
            })
            .collect()
    }

    /// Keep the tables a constructor's own emission recorded. `add_method` curates every `<init>`'s
    /// line and local tables from its class's declarations; a declared secondary constructor's are
    /// its body's, as for any method.
    pub fn keep_method_debug(&mut self, name: &str, desc: &str, code: &CodeBuilder) {
        let n = self
            .cp
            .lookup_utf8(name)
            .expect("a just-emitted method retains its interned name");
        let d = self
            .cp
            .lookup_utf8(desc)
            .expect("a just-emitted method retains its interned descriptor");
        let lvt = self.local_table(code);
        let method = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
            .expect("a just-emitted method must exist before retaining its debug tables");
        method.lnt = code.line_marks().to_vec();
        method.lvt = lvt;
    }

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
