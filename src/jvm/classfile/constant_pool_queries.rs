//! Read-only, non-interning views over the class writer's constant pool.
//!
//! Finished-method analyses use these after emission, when adding a constant merely to inspect an
//! operand would change classfile identity. Keeping the queries together also keeps the classfile
//! facade focused on construction and serialization.

use super::{Const, ConstPool, VerifType};
use crate::jvm::method_node::{Constant, ConstantPoolView, ConstantSink, Handle, PoolEntry};
use crate::jvm::names::classfile_internal_name;

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

/// The writer's pool as a finished method's rewrite reads and assembles against it, adding no
/// entry. A rewrite only moves operands the method already names, so each constant it asks for is
/// in the pool; a request for one that is not is remembered, and the rewrite is then declined.
pub(super) struct PoolLookup<'a> {
    pool: &'a ConstPool,
    bootstrap_methods: &'a [(u16, Vec<u16>)],
    missing: bool,
}

impl<'a> PoolLookup<'a> {
    pub(super) fn new(
        pool: &'a ConstPool,
        bootstrap_methods: &'a [(u16, Vec<u16>)],
    ) -> PoolLookup<'a> {
        PoolLookup {
            pool,
            bootstrap_methods,
            missing: false,
        }
    }

    /// Whether a constant was asked for that the pool does not hold.
    pub(super) fn missed(&self) -> bool {
        self.missing
    }

    fn find(&mut self, constant: Const) -> u16 {
        match self.pool.dedup.get(&constant) {
            Some(&index) => index,
            None => {
                self.missing = true;
                0
            }
        }
    }

    fn utf8(&mut self, text: &str) -> u16 {
        self.find(Const::Utf8(text.to_string()))
    }

    fn name_and_type(&mut self, name: &str, desc: &str) -> u16 {
        let (name, desc) = (self.utf8(name), self.utf8(desc));
        self.find(Const::NameAndType(name, desc))
    }

    fn handle(&mut self, handle: &Handle) -> u16 {
        let member = match handle.kind {
            1..=4 => self.field(&handle.owner, &handle.name, &handle.desc),
            _ => self.method(&handle.owner, &handle.name, &handle.desc, handle.interface),
        };
        self.find(Const::MethodHandle(handle.kind, member))
    }
}

impl ConstantPoolView for PoolLookup<'_> {
    fn entry(&self, index: u16) -> Option<PoolEntry<'_>> {
        Some(match self.pool.entry_at(index)? {
            Const::Utf8(text) => PoolEntry::Utf8(text),
            Const::Utf8Units(units) => PoolEntry::Utf8Units(units),
            Const::Integer(value) => PoolEntry::Integer(*value),
            Const::Float(bits) => PoolEntry::Float(*bits),
            Const::Long(value) => PoolEntry::Long(*value),
            Const::Double(bits) => PoolEntry::Double(*bits),
            Const::Class(name) => PoolEntry::Class(*name),
            Const::String(value) => PoolEntry::String(*value),
            Const::NameAndType(name, desc) => PoolEntry::NameAndType(*name, *desc),
            Const::Methodref(class, signature) => PoolEntry::Methodref(*class, *signature),
            Const::InterfaceMethodref(class, signature) => {
                PoolEntry::InterfaceMethodref(*class, *signature)
            }
            Const::Fieldref(class, signature) => PoolEntry::Fieldref(*class, *signature),
            Const::MethodHandle(kind, member) => PoolEntry::MethodHandle(*kind, *member),
            Const::MethodType(desc) => PoolEntry::MethodType(*desc),
            Const::InvokeDynamic(bootstrap, signature) => {
                PoolEntry::InvokeDynamic(*bootstrap, *signature)
            }
        })
    }

    fn bootstrap_method(&self, index: u16) -> Option<(u16, &[u16])> {
        self.bootstrap_methods
            .get(usize::from(index))
            .map(|(handle, arguments)| (*handle, arguments.as_slice()))
    }
}

impl ConstantSink for PoolLookup<'_> {
    fn class(&mut self, name: &str) -> u16 {
        let name = self.utf8(&classfile_internal_name(name));
        self.find(Const::Class(name))
    }

    fn field(&mut self, owner: &str, name: &str, desc: &str) -> u16 {
        let (owner, signature) = (self.class(owner), self.name_and_type(name, desc));
        self.find(Const::Fieldref(owner, signature))
    }

    fn method(&mut self, owner: &str, name: &str, desc: &str, interface: bool) -> u16 {
        let (owner, signature) = (self.class(owner), self.name_and_type(name, desc));
        self.find(if interface {
            Const::InterfaceMethodref(owner, signature)
        } else {
            Const::Methodref(owner, signature)
        })
    }

    fn constant(&mut self, constant: &Constant) -> u16 {
        match constant {
            Constant::Int(value) => self.find(Const::Integer(*value)),
            Constant::Float(bits) => self.find(Const::Float(*bits)),
            Constant::Long(value) => self.find(Const::Long(*value)),
            Constant::Double(bits) => self.find(Const::Double(*bits)),
            Constant::String(value) => {
                let text = match value.as_str() {
                    Some(text) => self.utf8(text),
                    None => self.find(Const::Utf8Units(value.units().collect())),
                };
                self.find(Const::String(text))
            }
            Constant::Class(name) => self.class(name),
            Constant::MethodType(desc) => {
                let desc = self.utf8(desc);
                self.find(Const::MethodType(desc))
            }
            Constant::Handle(handle) => self.handle(handle),
        }
    }

    fn invoke_dynamic(
        &mut self,
        name: &str,
        desc: &str,
        bootstrap: &Handle,
        arguments: &[Constant],
    ) -> u16 {
        let handle = self.handle(bootstrap);
        let arguments: Vec<u16> = arguments
            .iter()
            .map(|argument| self.constant(argument))
            .collect();
        let entry = self
            .bootstrap_methods
            .iter()
            .position(|(known, known_arguments)| *known == handle && *known_arguments == arguments);
        let Some(entry) = entry else {
            self.missing = true;
            return 0;
        };
        let signature = self.name_and_type(name, desc);
        self.find(Const::InvokeDynamic(entry as u16, signature))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_pool_view_uses_the_authoritative_slot_index() {
        let mut pool = ConstPool::default();
        let wide = pool.intern(Const::Long(5));
        let following = pool.utf8("following");
        let view = PoolLookup::new(&pool, &[]);

        assert!(matches!(view.entry(wide), Some(PoolEntry::Long(5))));
        assert!(
            view.entry(wide + 1).is_none(),
            "wide second slot is unusable"
        );
        assert!(matches!(
            view.entry(following),
            Some(PoolEntry::Utf8("following"))
        ));
    }
}
