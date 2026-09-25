//! Read-only, non-interning views over the class writer's constant pool.
//!
//! Finished-method analyses use these after emission, when adding a constant merely to inspect an
//! operand would change classfile identity. Keeping the queries together also keeps the classfile
//! facade focused on construction and serialization.

use super::{Const, ConstPool, VerifType};

impl ConstPool {
    /// Non-interning lookup of an existing `CONSTANT_Utf8` entry.
    pub(super) fn lookup_utf8(&self, text: &str) -> Option<u16> {
        self.dedup.get(&Const::Utf8(text.to_string())).copied()
    }

    /// Non-interning lookup of an existing `CONSTANT_String` entry.
    pub(super) fn lookup_string(&self, text: &str) -> Option<u16> {
        let utf8 = self.dedup.get(&Const::Utf8(text.to_string())).copied()?;
        self.dedup.get(&Const::String(utf8)).copied()
    }

    /// The entry at 1-based pool index `idx` (long/double occupy two slots).
    pub(super) fn entry_at(&self, idx: u16) -> Option<&Const> {
        let entry = *self.slot_entries.get(usize::from(idx).checked_sub(1)?)?;
        self.entries.get(entry as usize)
    }

    /// The internal name of the `CONSTANT_Class` at `idx`.
    pub(super) fn class_name(&self, idx: u16) -> Option<&str> {
        let Const::Class(utf8_idx) = self.entry_at(idx)? else {
            return None;
        };
        match self.entry_at(*utf8_idx)? {
            Const::Utf8(name) => Some(name),
            _ => None,
        }
    }

    /// The text of the `CONSTANT_Utf8` at `idx`.
    pub(super) fn utf8_at(&self, idx: u16) -> Option<&str> {
        match self.entry_at(idx)? {
            Const::Utf8(text) => Some(text),
            _ => None,
        }
    }

    /// The `(owner, name, descriptor)` named by a method reference.
    pub(super) fn methodref_parts(&self, idx: u16) -> Option<(&str, &str, &str)> {
        let (class_idx, name_and_type) = match self.entry_at(idx)? {
            Const::Methodref(class, name_and_type)
            | Const::InterfaceMethodref(class, name_and_type) => (*class, *name_and_type),
            _ => return None,
        };
        let Const::NameAndType(name_idx, descriptor_idx) = self.entry_at(name_and_type)? else {
            return None;
        };
        Some((
            self.class_name(class_idx)?,
            self.utf8_at(*name_idx)?,
            self.utf8_at(*descriptor_idx)?,
        ))
    }

    /// The `(owner, name, descriptor)` named by a field reference.
    pub(super) fn fieldref_parts(&self, idx: u16) -> Option<(&str, &str, &str)> {
        let Const::Fieldref(class_idx, name_and_type) = self.entry_at(idx)? else {
            return None;
        };
        let Const::NameAndType(name_idx, descriptor_idx) = self.entry_at(*name_and_type)? else {
            return None;
        };
        Some((
            self.class_name(*class_idx)?,
            self.utf8_at(*name_idx)?,
            self.utf8_at(*descriptor_idx)?,
        ))
    }

    /// The descriptor named by a field reference.
    pub(super) fn fieldref_descriptor(&self, idx: u16) -> Option<&str> {
        let Const::Fieldref(_, name_and_type) = self.entry_at(idx)? else {
            return None;
        };
        let Const::NameAndType(_, descriptor_idx) = self.entry_at(*name_and_type)? else {
            return None;
        };
        self.utf8_at(*descriptor_idx)
    }

    /// The descriptor of an `invokedynamic` call site.
    pub(super) fn invokedynamic_descriptor(&self, idx: u16) -> Option<&str> {
        let Const::InvokeDynamic(_, name_and_type) = self.entry_at(idx)? else {
            return None;
        };
        let Const::NameAndType(_, descriptor_idx) = self.entry_at(*name_and_type)? else {
            return None;
        };
        self.utf8_at(*descriptor_idx)
    }

    /// The verification type pushed by an `ldc` family instruction.
    pub(super) fn loadable_constant_type(&self, idx: u16) -> Option<VerifType> {
        Some(match self.entry_at(idx)? {
            Const::Integer(_) => VerifType::Integer,
            Const::Float(_) => VerifType::Float,
            Const::Long(_) => VerifType::Long,
            Const::Double(_) => VerifType::Double,
            Const::String(_) => VerifType::ObjectName("java/lang/String".to_string()),
            Const::Class(_) => VerifType::ObjectName("java/lang/Class".to_string()),
            Const::MethodType(_) => {
                VerifType::ObjectName("java/lang/invoke/MethodType".to_string())
            }
            Const::MethodHandle(..) => {
                VerifType::ObjectName("java/lang/invoke/MethodHandle".to_string())
            }
            _ => return None,
        })
    }
}
