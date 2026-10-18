//! A method this writer emitted, read back as the body an inliner reads from a class file.
//!
//! kotlinc inlines a same-module inline function from the method node it compiled for it
//! (`IrSourceCompilerForInline`), not from the class file it later writes: that node has the
//! function's code, lines and locals, and the source map of the class it was compiled in, but none
//! of the bytecode rewrites that run when the class is written. This is that node in the form the
//! inliner reads a dependency's body in.

use std::sync::Arc;

use super::{ClassWriter, Const};
use crate::jvm::classreader::{ExcEntry, MethodCode, MethodLocal, C};
use crate::jvm::source_map::DependencyMap;

/// The one method a writer holds, with its body.
pub(crate) struct EmittedMethod {
    pub(crate) name: String,
    pub(crate) descriptor: String,
    pub(crate) body: MethodCode,
}

impl ClassWriter {
    /// The writer's only method, as emitted, before the rewrites that run when the class is
    /// written. `None` unless the writer declares exactly one method and it has a body whose
    /// debug tables and source map read back.
    pub(crate) fn sole_emitted_method(&self) -> Option<EmittedMethod> {
        let [method] = self.methods.as_slice() else {
            return None;
        };
        let code = method.code.clone()?;
        let code_length = u16::try_from(code.len()).ok()?;
        let source_cp = std::iter::once(Some(C::Other))
            .chain(
                (1..=self.cp.slot_count()).map(|slot| match self.cp.entry_at(slot) {
                    Some(entry) => Some(pool_entry(entry)),
                    // The second slot of a `long` or `double` names no entry.
                    None => Some(C::Other),
                }),
            )
            .collect::<Option<Vec<_>>>()?;
        let locals = method
            .lvt
            .iter()
            .map(|&(name, desc, slot, start, length)| {
                let start_pc = start.unwrap_or(0);
                Some(MethodLocal {
                    start_pc,
                    length: length.unwrap_or(code_length.checked_sub(start_pc)?),
                    slot,
                    name: self.cp.utf8_at(name)?.to_string(),
                    descriptor: self.cp.utf8_at(desc)?.to_string(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let dependency_source_map = match self.source_map.render() {
            Some(text) => Some(Arc::new(DependencyMap::parse(&text)?)),
            None => None,
        };
        let body = MethodCode {
            max_stack: method.max_stack,
            max_locals: method.max_locals,
            code,
            source_cp: source_cp.into(),
            stackmap: None,
            handlers: method
                .exceptions
                .iter()
                .map(|&(start_pc, end_pc, handler_pc, catch_type)| ExcEntry {
                    start_pc,
                    end_pc,
                    handler_pc,
                    catch_type,
                })
                .collect(),
            locals,
            lines: method.lnt.clone(),
            source_file: self.source_file.clone(),
            defining_class: self.internal_name.clone(),
            dependency_source_map,
            bootstrap_methods: self.bootstrap_methods.clone(),
        };
        Some(EmittedMethod {
            name: self.cp.utf8_at(method.name)?.to_string(),
            descriptor: self.cp.utf8_at(method.desc)?.to_string(),
            body,
        })
    }
}

/// The class-file reader's form of one pool entry.
fn pool_entry(entry: &Const) -> C {
    match entry {
        Const::Utf8(text) => C::Utf8(text.clone()),
        Const::Utf8Units(units) => C::Utf8Units(units.clone()),
        Const::Integer(value) => C::Integer(*value),
        Const::Float(bits) => C::Float(*bits),
        Const::Long(value) => C::Long(*value),
        Const::Double(bits) => C::Double(*bits),
        Const::Class(name) => C::Class(*name),
        Const::String(value) => C::String(*value),
        Const::NameAndType(name, desc) => C::NameAndType(*name, *desc),
        Const::Methodref(class, member) => C::Methodref(*class, *member),
        Const::InterfaceMethodref(class, member) => C::InterfaceMethodref(*class, *member),
        Const::Fieldref(class, member) => C::Fieldref(*class, *member),
        Const::MethodHandle(kind, member) => C::MethodHandle(*kind, *member),
        Const::MethodType(desc) => C::MethodType(*desc),
        Const::InvokeDynamic(bootstrap, member) => C::InvokeDynamic(*bootstrap, *member),
    }
}
