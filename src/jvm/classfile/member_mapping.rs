//! Which member references the class's own code names, as opposed to code copied from an inline
//! function's compiled body.
//!
//! kotlinc lists a nested class in `InnerClasses` when its type mapper maps a signature naming it
//! while generating the class. The instructions its inliner copies from a compiled inline function
//! (`Continuation.resume`'s `Result.Companion` read) never pass through that mapper, so a nested
//! class only such code names gets no row. A member reference interned while copying is recorded
//! as copied; one the class's own code interns is recorded as mapped.

use std::collections::HashSet;

use super::{ClassWriter, Const};

/// The member references interned while copying compiled code, and those interned otherwise.
#[derive(Default)]
pub(super) struct MappedMembers {
    copying: bool,
    copied: HashSet<u16>,
    mapped: HashSet<u16>,
}

impl MappedMembers {
    fn record(&mut self, index: u16) -> u16 {
        if self.copying {
            self.copied.insert(index);
        } else {
            self.mapped.insert(index);
        }
        index
    }

    /// How many references the class's own code names; the mention cache is rebuilt when it
    /// grows.
    pub(super) fn mapped_count(&self) -> usize {
        self.mapped.len()
    }

    /// Whether only copied code names the member reference at `index`.
    pub(super) fn copied_only(&self, index: u16) -> bool {
        self.copied.contains(&index) && !self.mapped.contains(&index)
    }
}

impl ClassWriter {
    pub fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let index = self.cp.methodref(class, name, desc);
        self.members.record(index)
    }

    pub fn interface_methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let index = self.cp.interface_methodref(class, name, desc);
        self.members.record(index)
    }

    pub fn fieldref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let index = self.cp.fieldref(class, name, desc);
        self.members.record(index)
    }

    /// Run `copy`, which writes instructions copied from a compiled inline function's body, with
    /// every member reference it interns recorded as copied.
    pub(crate) fn copying<T>(&mut self, copy: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.set_copying(true);
        let result = copy(self);
        self.set_copying(outer);
        result
    }

    /// Record the member references interned from now on as copied (`true`) or as the class's own
    /// (`false`); returns the previous setting so the caller can restore it.
    pub(crate) fn set_copying(&mut self, copying: bool) -> bool {
        std::mem::replace(&mut self.members.copying, copying)
    }

    /// The `NameAndType` entries only copied member references use: their descriptors were never
    /// mapped for this class.
    pub(super) fn copied_only_name_and_types(&self) -> HashSet<u16> {
        let mut copied = HashSet::new();
        let mut used = HashSet::new();
        for slot in 1..=self.cp.slot_entries.len() {
            let index = u16::try_from(slot).expect("a constant pool index fits in u16");
            let Some(
                Const::Methodref(_, name_and_type)
                | Const::InterfaceMethodref(_, name_and_type)
                | Const::Fieldref(_, name_and_type),
            ) = self.cp.entry_at(index)
            else {
                continue;
            };
            if self.members.copied_only(index) {
                copied.insert(*name_and_type);
            } else {
                used.insert(*name_and_type);
            }
        }
        copied.retain(|name_and_type| !used.contains(name_and_type));
        copied
    }
}

#[cfg(test)]
mod tests {
    use super::ClassWriter;

    /// A reference only copied code interns is copied; the class's own use of the same reference,
    /// before or after the copy, makes it mapped, and so does its descriptor's `NameAndType`.
    #[test]
    fn a_reference_is_copied_only_until_the_class_names_it_itself() {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let copied = writer.copying(|writer| writer.fieldref("H", "item", "LH$Item;"));
        let shared = writer.copying(|writer| writer.methodref("H", "make", "()LH$Item;"));
        assert!(writer.members.copied_only(copied));
        assert!(writer.members.copied_only(shared));
        assert_eq!(writer.copied_only_name_and_types().len(), 2);

        assert_eq!(writer.methodref("H", "make", "()LH$Item;"), shared);
        assert!(writer.members.copied_only(copied));
        assert!(!writer.members.copied_only(shared));
        assert_eq!(writer.copied_only_name_and_types().len(), 1);
    }
}
