//! Resolving a body's constant-pool operands to their symbolic form.
//!
//! A body is decoded against the pool of the class it was written into. That pool is either one
//! read from a class file (a [`MethodCode`]'s) or the pool of a class krusty is still writing; both
//! show their entries through [`ConstantPoolView`], so the reader does not care which it has.

use super::nodes::{Constant, Handle, Insn};
use super::read::{malformed, MalformedCode};
use crate::jvm::classreader::{MethodCode, C};
use crate::kt_string::KtString;

/// One constant-pool entry as a reader sees it, its references still pool indices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PoolEntry<'a> {
    Utf8(&'a str),
    /// A `CONSTANT_Utf8` no Rust string can spell (it holds an unpaired surrogate), as its UTF-16
    /// code units.
    Utf8Units(&'a [u16]),
    Integer(i32),
    Float(u32),
    Long(i64),
    Double(u64),
    Class(u16),
    String(u16),
    NameAndType(u16, u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    MethodHandle(u8, u16),
    MethodType(u16),
    InvokeDynamic(u16, u16),
}

/// A constant pool and its class's `BootstrapMethods`, as a body is decoded against them.
pub trait ConstantPoolView {
    /// The entry at 1-based `index`, or `None` for an unused or out-of-range index.
    fn entry(&self, index: u16) -> Option<PoolEntry<'_>>;
    /// The bootstrap method at `index`: its `MethodHandle` and its static arguments.
    fn bootstrap_method(&self, index: u16) -> Option<(u16, &[u16])>;
}

/// A class file's pool entry as the reader sees it; `None` for the unused slot after a `long` or
/// `double`, and for the entries no instruction names.
pub(super) fn class_file_entry(entry: &C) -> Option<PoolEntry<'_>> {
    Some(match entry {
        C::Utf8(text) => PoolEntry::Utf8(text),
        C::Utf8Units(units) => PoolEntry::Utf8Units(units),
        C::Class(name) => PoolEntry::Class(*name),
        C::NameAndType(name, desc) => PoolEntry::NameAndType(*name, *desc),
        C::Fieldref(class, signature) => PoolEntry::Fieldref(*class, *signature),
        C::Methodref(class, signature) => PoolEntry::Methodref(*class, *signature),
        C::InterfaceMethodref(class, signature) => {
            PoolEntry::InterfaceMethodref(*class, *signature)
        }
        C::String(value) => PoolEntry::String(*value),
        C::Integer(value) => PoolEntry::Integer(*value),
        C::Float(bits) => PoolEntry::Float(*bits),
        C::Long(value) => PoolEntry::Long(*value),
        C::Double(bits) => PoolEntry::Double(*bits),
        C::MethodHandle(kind, member) => PoolEntry::MethodHandle(*kind, *member),
        C::MethodType(desc) => PoolEntry::MethodType(*desc),
        C::InvokeDynamic(bootstrap, signature) => PoolEntry::InvokeDynamic(*bootstrap, *signature),
        C::Other => return None,
    })
}

impl ConstantPoolView for MethodCode {
    fn entry(&self, index: u16) -> Option<PoolEntry<'_>> {
        class_file_entry(self.source_cp.get(usize::from(index))?)
    }

    fn bootstrap_method(&self, index: u16) -> Option<(u16, &[u16])> {
        self.bootstrap_methods
            .get(usize::from(index))
            .map(|(handle, arguments)| (*handle, arguments.as_slice()))
    }
}

/// A pool being decoded against, with the position being decoded for error reports.
pub(super) struct SourcePool<'a, P: ?Sized> {
    pub(super) pool: &'a P,
    /// The instruction or table entry being decoded.
    pub(super) at: usize,
}

impl<P: ConstantPoolView + ?Sized> SourcePool<'_, P> {
    pub(super) fn fail(&self, reason: &'static str) -> MalformedCode {
        malformed(self.at, reason)
    }

    fn utf8(&self, index: u16) -> Result<&str, MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::Utf8(text)) => Ok(text),
            _ => Err(self.fail("expected a Utf8 constant")),
        }
    }

    pub(super) fn class(&self, index: u16) -> Result<String, MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::Class(name)) => Ok(self.utf8(name)?.to_string()),
            _ => Err(self.fail("expected a Class constant")),
        }
    }

    fn name_and_type(&self, index: u16) -> Result<(String, String), MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::NameAndType(name, desc)) => {
                Ok((self.utf8(name)?.to_string(), self.utf8(desc)?.to_string()))
            }
            _ => Err(self.fail("expected a NameAndType constant")),
        }
    }

    /// A field or method reference: `(owner, name, desc, is InterfaceMethodref)`.
    fn member(&self, index: u16) -> Result<(String, String, String, bool), MalformedCode> {
        let (class, signature, interface) = match self.pool.entry(index) {
            Some(PoolEntry::Fieldref(class, signature))
            | Some(PoolEntry::Methodref(class, signature)) => (class, signature, false),
            Some(PoolEntry::InterfaceMethodref(class, signature)) => (class, signature, true),
            _ => return Err(self.fail("expected a member reference")),
        };
        let (name, desc) = self.name_and_type(signature)?;
        Ok((self.class(class)?, name, desc, interface))
    }

    pub(super) fn field(&self, index: u16) -> Result<(String, String, String), MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::Fieldref(..)) => {
                let (owner, name, desc, _) = self.member(index)?;
                Ok((owner, name, desc))
            }
            _ => Err(self.fail("expected a Fieldref constant")),
        }
    }

    pub(super) fn method(
        &self,
        index: u16,
    ) -> Result<(String, String, String, bool), MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::Methodref(..)) | Some(PoolEntry::InterfaceMethodref(..)) => {
                self.member(index)
            }
            _ => Err(self.fail("expected a method reference")),
        }
    }

    fn handle(&self, index: u16) -> Result<Handle, MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::MethodHandle(kind, member)) => {
                let (owner, name, desc, interface) = self.member(member)?;
                Ok(Handle {
                    kind,
                    owner,
                    name,
                    desc,
                    interface,
                })
            }
            _ => Err(self.fail("expected a MethodHandle constant")),
        }
    }

    fn string(&self, index: u16) -> Result<KtString, MalformedCode> {
        match self.pool.entry(index) {
            Some(PoolEntry::Utf8(text)) => Ok(KtString::from(text.to_string())),
            Some(PoolEntry::Utf8Units(units)) => Ok(KtString::from_units(units.to_vec())),
            _ => Err(self.fail("malformed String constant")),
        }
    }

    pub(super) fn constant(&self, index: u16) -> Result<Constant, MalformedCode> {
        Ok(match self.pool.entry(index) {
            Some(PoolEntry::Integer(value)) => Constant::Int(value),
            Some(PoolEntry::Float(bits)) => Constant::Float(bits),
            Some(PoolEntry::Long(value)) => Constant::Long(value),
            Some(PoolEntry::Double(bits)) => Constant::Double(bits),
            Some(PoolEntry::String(value)) => Constant::String(self.string(value)?),
            Some(PoolEntry::Class(_)) => Constant::Class(self.class(index)?),
            Some(PoolEntry::MethodType(desc)) => Constant::MethodType(self.utf8(desc)?.to_string()),
            Some(PoolEntry::MethodHandle(..)) => Constant::Handle(self.handle(index)?),
            _ => return Err(self.fail("expected a loadable constant")),
        })
    }

    pub(super) fn invoke_dynamic(&self, index: u16) -> Result<Insn, MalformedCode> {
        let Some(PoolEntry::InvokeDynamic(bootstrap, signature)) = self.pool.entry(index) else {
            return Err(self.fail("expected an InvokeDynamic constant"));
        };
        let (name, desc) = self.name_and_type(signature)?;
        let (handle, arguments) = self
            .pool
            .bootstrap_method(bootstrap)
            .ok_or_else(|| self.fail("bootstrap method index out of range"))?;
        Ok(Insn::InvokeDynamic {
            name,
            desc,
            bootstrap: self.handle(handle)?,
            arguments: arguments
                .iter()
                .map(|&argument| self.constant(argument))
                .collect::<Result<_, _>>()?,
        })
    }
}
