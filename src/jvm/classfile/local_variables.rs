//! A method's `LocalVariableTable` strings: when its names and descriptors intern.

use super::{ClassWriter, CodeBuilder};

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
