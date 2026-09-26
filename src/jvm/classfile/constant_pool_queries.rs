//! Read-only, non-interning views over the class writer's constant pool.
//!
//! Finished-method analyses use these after emission, when adding a constant merely to inspect an
//! operand would change classfile identity. Keeping the queries together also keeps the classfile
//! facade focused on construction and serialization.

use super::{ClassWriter, Const, ConstPool, VerifType};
use crate::jvm::method_node::{
    CodeAttribute, Constant, ConstantPoolView, ConstantSink, Handle, MethodNode, PoolEntry,
};
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

/// A constant a rewritten body names that the pool did not hold when it was laid out, as the
/// writer interns it (see [`PoolLookup::into_wanted`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Wanted {
    Class(String),
    Field(String, String, String),
    Method(String, String, String, bool),
    Constant(Constant),
    /// A local variable's descriptor, which only the local-variable table names.
    Utf8(String),
}

/// The writer's pool as a finished method's rewrite reads and assembles against it, adding no
/// entry. Each constant asked for that the pool does not hold is remembered as [`Wanted`], and the
/// layout that asked for it is not usable: the caller interns what was wanted and lays the body out
/// again, or declines the rewrite.
pub(super) struct PoolLookup<'a> {
    pool: &'a ConstPool,
    bootstrap_methods: &'a [(u16, Vec<u16>)],
    wanted: Vec<Wanted>,
    /// A constant was asked for that cannot be named by a [`Wanted`] (a new call site).
    unnameable: bool,
}

impl<'a> PoolLookup<'a> {
    pub(super) fn new(
        pool: &'a ConstPool,
        bootstrap_methods: &'a [(u16, Vec<u16>)],
    ) -> PoolLookup<'a> {
        PoolLookup {
            pool,
            bootstrap_methods,
            wanted: Vec::new(),
            unnameable: false,
        }
    }

    /// Whether a constant was asked for that the pool does not hold.
    pub(super) fn missed(&self) -> bool {
        self.unnameable || !self.wanted.is_empty()
    }

    /// The constants asked for that the pool does not hold, in the order they were asked for;
    /// `None` when one of them cannot be interned on its own.
    pub(super) fn into_wanted(self) -> Option<Vec<Wanted>> {
        (!self.unnameable).then_some(self.wanted)
    }

    /// The `CONSTANT_Utf8` holding a local variable's descriptor.
    pub(super) fn descriptor(&mut self, desc: &str) -> u16 {
        let index = self.pool.lookup_utf8(desc);
        self.wanted_unless(index, || Wanted::Utf8(desc.to_string()))
    }

    /// `index`, or `0` with `wanted` remembered when the pool does not hold the constant.
    fn wanted_unless(&mut self, index: Option<u16>, wanted: impl FnOnce() -> Wanted) -> u16 {
        index.unwrap_or_else(|| {
            self.wanted.push(wanted());
            0
        })
    }

    fn find(&self, constant: Const) -> Option<u16> {
        self.pool.dedup.get(&constant).copied()
    }

    fn utf8(&self, text: &str) -> Option<u16> {
        self.pool.lookup_utf8(text)
    }

    fn name_and_type(&self, name: &str, desc: &str) -> Option<u16> {
        self.find(Const::NameAndType(self.utf8(name)?, self.utf8(desc)?))
    }

    fn class_index(&self, name: &str) -> Option<u16> {
        self.find(Const::Class(self.utf8(&classfile_internal_name(name))?))
    }

    fn member(&self, owner: &str, name: &str, desc: &str, kind: MemberKind) -> Option<u16> {
        let (owner, signature) = (self.class_index(owner)?, self.name_and_type(name, desc)?);
        self.find(match kind {
            MemberKind::Field => Const::Fieldref(owner, signature),
            MemberKind::Method => Const::Methodref(owner, signature),
            MemberKind::InterfaceMethod => Const::InterfaceMethodref(owner, signature),
        })
    }

    fn handle_index(&self, handle: &Handle) -> Option<u16> {
        let kind = match handle.kind {
            1..=4 => MemberKind::Field,
            _ if handle.interface => MemberKind::InterfaceMethod,
            _ => MemberKind::Method,
        };
        let member = self.member(&handle.owner, &handle.name, &handle.desc, kind)?;
        self.find(Const::MethodHandle(handle.kind, member))
    }

    fn constant_index(&self, constant: &Constant) -> Option<u16> {
        match constant {
            Constant::Int(value) => self.find(Const::Integer(*value)),
            Constant::Float(bits) => self.find(Const::Float(*bits)),
            Constant::Long(value) => self.find(Const::Long(*value)),
            Constant::Double(bits) => self.find(Const::Double(*bits)),
            Constant::String(value) => {
                let text = match value.as_str() {
                    Some(text) => self.utf8(text)?,
                    None => self.find(Const::Utf8Units(value.units().collect()))?,
                };
                self.find(Const::String(text))
            }
            Constant::Class(name) => self.class_index(name),
            Constant::MethodType(desc) => self.find(Const::MethodType(self.utf8(desc)?)),
            Constant::Handle(handle) => self.handle_index(handle),
        }
    }
}

/// Which reference constant a member names.
#[derive(Clone, Copy)]
enum MemberKind {
    Field,
    Method,
    InterfaceMethod,
}

impl ClassWriter {
    /// Code this writer's builder emitted, read into a node against the writer's pool, which holds
    /// every constant it names. `None` when the code cannot be decoded.
    pub(crate) fn read_emitted_code(
        &self,
        access: u16,
        name: &str,
        desc: &str,
        code: &CodeAttribute<'_>,
    ) -> Option<MethodNode> {
        let pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        MethodNode::read_code(access, name, desc, code, &pool).ok()
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
        let index = self.class_index(name);
        self.wanted_unless(index, || Wanted::Class(name.to_string()))
    }

    fn field(&mut self, owner: &str, name: &str, desc: &str) -> u16 {
        let index = self.member(owner, name, desc, MemberKind::Field);
        self.wanted_unless(index, || {
            Wanted::Field(owner.to_string(), name.to_string(), desc.to_string())
        })
    }

    fn method(&mut self, owner: &str, name: &str, desc: &str, interface: bool) -> u16 {
        let kind = if interface {
            MemberKind::InterfaceMethod
        } else {
            MemberKind::Method
        };
        let index = self.member(owner, name, desc, kind);
        self.wanted_unless(index, || {
            Wanted::Method(
                owner.to_string(),
                name.to_string(),
                desc.to_string(),
                interface,
            )
        })
    }

    fn constant(&mut self, constant: &Constant) -> u16 {
        let index = self.constant_index(constant);
        self.wanted_unless(index, || Wanted::Constant(constant.clone()))
    }

    fn invoke_dynamic(
        &mut self,
        name: &str,
        desc: &str,
        bootstrap: &Handle,
        arguments: &[Constant],
    ) -> u16 {
        let site = (|| {
            let handle = self.handle_index(bootstrap)?;
            let arguments: Vec<u16> = arguments
                .iter()
                .map(|argument| self.constant_index(argument))
                .collect::<Option<_>>()?;
            let entry = self
                .bootstrap_methods
                .iter()
                .position(|(known, known_arguments)| {
                    *known == handle && *known_arguments == arguments
                })?;
            let signature = self.name_and_type(name, desc)?;
            self.find(Const::InvokeDynamic(u16::try_from(entry).ok()?, signature))
        })();
        site.unwrap_or_else(|| {
            self.unnameable = true;
            0
        })
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

    #[test]
    fn a_constant_the_pool_lacks_is_wanted_and_found_once_interned() {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let mut lookup = PoolLookup::new(&writer.cp, &writer.bootstrap_methods);
        assert_eq!(lookup.class("T"), writer.this_class);
        assert!(!lookup.missed());
        assert_eq!(lookup.field("T", "x", "I"), 0);
        assert_eq!(lookup.descriptor("J"), 0);
        assert!(lookup.missed());
        let wanted = lookup.into_wanted().expect("every constant can be named");
        assert_eq!(
            wanted,
            [
                Wanted::Field("T".to_string(), "x".to_string(), "I".to_string()),
                Wanted::Utf8("J".to_string()),
            ]
        );
        let field = writer.field("T", "x", "I");
        let descriptor = writer.cp.utf8("J");
        let mut lookup = PoolLookup::new(&writer.cp, &writer.bootstrap_methods);
        assert_eq!(lookup.field("T", "x", "I"), field);
        assert_eq!(lookup.descriptor("J"), descriptor);
        assert!(!lookup.missed());
    }

    #[test]
    fn a_call_site_the_pool_lacks_cannot_be_wanted() {
        let writer = ClassWriter::new("T", "java/lang/Object");
        let mut lookup = PoolLookup::new(&writer.cp, &writer.bootstrap_methods);
        let bootstrap = Handle {
            kind: 6,
            owner: "B".to_string(),
            name: "bootstrap".to_string(),
            desc: "()V".to_string(),
            interface: false,
        };
        assert_eq!(lookup.invoke_dynamic("run", "()V", &bootstrap, &[]), 0);
        assert!(lookup.missed());
        assert_eq!(lookup.into_wanted(), None);
    }
}
