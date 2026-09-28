//! A method's `LocalVariableTable` strings: when its names and descriptors intern.

use super::{ClassWriter, CodeBuilder, LvtEntry};

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

/// A method body's own `LocalVariableTable` rows, interned where the body ends, for a method whose
/// receiver and parameter rows are attached later.
pub struct BodyLocals(Vec<LvtEntry>);

impl ClassWriter {
    /// Intern the locals `code` recorded now, where kotlinc visits them: right after the body.
    pub fn intern_body_locals(&mut self, code: &CodeBuilder) -> BodyLocals {
        BodyLocals(self.local_table(code))
    }

    /// Put a body's locals ahead of the rows already attached to `name desc`, as kotlinc orders a
    /// method's table: the body's locals, then the receiver and parameters.
    pub fn prepend_body_locals(&mut self, name: &str, desc: &str, locals: BodyLocals) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            method.lvt.splice(0..0, locals.0);
        }
    }
}
