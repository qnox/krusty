//! Which member references and class constants the class's own code names, as opposed to code
//! copied from an inline function's compiled body.
//!
//! kotlinc lists a nested class in `InnerClasses` when its type mapper maps a signature or class
//! naming it while generating the class. The instructions its inliner copies from a compiled inline
//! function (`Continuation.resume`'s `Result.Companion` read, a `checkcast` to a nested class) never
//! pass through that mapper, and neither do the ones its coroutine transformer adds with raw ASM (a
//! spilled local's `checkcast` on restore), so a nested class only such code names gets no row. A
//! reference interned while copying is recorded as copied; one the class's own code interns is
//! recorded as mapped.

use std::collections::HashSet;

use super::{ClassWriter, Const, ConstPool};

/// The member references interned while copying compiled code, and those interned otherwise.
#[derive(Default)]
pub(super) struct MappedMembers {
    copying: bool,
    copied: HashSet<u16>,
    mapped: HashSet<u16>,
    /// Class constants copying created; one that existed before (the class's own name, its
    /// supertypes) was not created by the copy.
    copied_classes: HashSet<u16>,
    mapped_classes: HashSet<u16>,
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

    /// Whether only copied code names the class constant at `index`.
    pub(super) fn copied_only_class(&self, index: u16) -> bool {
        self.copied_classes.contains(&index) && !self.mapped_classes.contains(&index)
    }
}

impl ClassWriter {
    /// Constant-pool index of a `Class` entry, for `new`, `checkcast` and the like.
    pub fn class_ref(&mut self, internal: &str) -> u16 {
        self.with_class_origin(internal, |pool| pool.class(internal))
    }

    /// Whether the class's own code, or a declaration, names `internal` as a class constant.
    pub(super) fn names_class(&self, internal: &str) -> bool {
        self.cp
            .class_index(internal)
            .is_some_and(|index| !self.members.copied_only_class(index))
    }

    pub fn methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let index = self.with_class_origin(class, |pool| pool.methodref(class, name, desc));
        self.members.record(index)
    }

    pub fn interface_methodref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let index =
            self.with_class_origin(class, |pool| pool.interface_methodref(class, name, desc));
        self.members.record(index)
    }

    pub fn fieldref(&mut self, class: &str, name: &str, desc: &str) -> u16 {
        let index = self.with_class_origin(class, |pool| pool.fieldref(class, name, desc));
        self.members.record(index)
    }

    /// Run `intern`, which interns `class`'s `CONSTANT_Class` (itself or as a member's owner), and
    /// record that class constant's origin: the class's own code maps it, while copied code
    /// creates it only if no entry existed before.
    fn with_class_origin(
        &mut self,
        class: &str,
        intern: impl FnOnce(&mut ConstPool) -> u16,
    ) -> u16 {
        // The pool interns the physical classfile name (`kotlin/Double$Companion` is
        // `kotlin/jvm/internal/DoubleCompanionObject`), so the origin is that entry's.
        let physical = crate::jvm::names::classfile_internal_name(class);
        let created = !self.cp.has_class(&physical);
        let index = intern(&mut self.cp);
        let class = self
            .cp
            .class_index(&physical)
            .expect("interning a class constant or a member of the class adds its class entry");
        if !self.members.copying {
            self.members.mapped_classes.insert(class);
        } else if created {
            self.members.copied_classes.insert(class);
        }
        index
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

    /// A class constant copied code creates is not named by the class until its own code interns
    /// it; one that existed before the copy, such as a supertype, stays named.
    #[test]
    fn a_class_constant_is_copied_only_when_the_copy_created_it() {
        let mut writer = ClassWriter::new("T", "S$Base");
        writer.copying(|writer| {
            writer.class_ref("H$Item");
            writer.class_ref("S$Base");
        });
        assert!(!writer.names_class("H$Item"));
        assert!(writer.names_class("S$Base"));

        writer.class_ref("H$Item");
        assert!(writer.names_class("H$Item"));
    }

    /// A member reference interns its owner's class constant, so a copied call on a nested owner
    /// names no class until the class's own code references that owner, through a member or not.
    #[test]
    fn a_copied_member_reference_creates_its_owner_as_copied() {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        writer.copying(|writer| {
            writer.methodref("H$Item", "touch", "()V");
            writer.interface_methodref("H$Shape", "area", "()I");
            writer.fieldref("H$Slot", "value", "I");
            writer.class_ref("H$Item");
        });
        for owner in ["H$Item", "H$Shape", "H$Slot"] {
            assert!(!writer.names_class(owner), "{owner}");
        }

        writer.fieldref("H$Item", "size", "I");
        writer.class_ref("H$Shape");
        assert!(writer.names_class("H$Item"));
        assert!(writer.names_class("H$Shape"));
        assert!(!writer.names_class("H$Slot"));
    }
}
