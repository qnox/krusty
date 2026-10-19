//! A method this writer emitted, read back as the body an inliner reads from a class file.
//!
//! kotlinc inlines a same-module inline function from the method node it compiled for it
//! (`IrSourceCompilerForInline`), not from the class file it later writes: that node has the
//! function's code, lines and locals, and the source map of the class it was compiled in, but none
//! of the bytecode rewrites that run when the class is written. This is that node in the form the
//! inliner reads a dependency's body in.

use std::sync::Arc;

use super::{ClassWriter, Const};
use crate::jvm::bytecode_passes::{descriptors, local_slots};
use crate::jvm::classreader::{ExcEntry, MethodCode, MethodLocal, C};
use crate::jvm::method_node::MethodNode;
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
    fn sole_emitted_method(&self) -> Option<EmittedMethod> {
        let [method] = self.methods.as_slice() else {
            return None;
        };
        let code = method.code.clone()?;
        let code_length = u16::try_from(code.len()).ok()?;
        let source_cp = self.reader_pool();
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
        // A method something was inlined into keeps its own map until the class is written; the
        // node carries that map, as kotlinc's method node carries its codegen's source mapper.
        let source_map = method.source_map.as_ref().unwrap_or(&self.source_map);
        let dependency_source_map = match source_map.render() {
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

impl ClassWriter {
    /// The pool as the class-file reader holds it, slot 0 included.
    fn reader_pool(&self) -> Vec<C> {
        std::iter::once(C::Other)
            .chain(
                (1..=self.cp.slot_count()).map(|slot| match self.cp.entry_at(slot) {
                    Some(entry) => pool_entry(entry),
                    // The second slot of a `long` or `double` names no entry.
                    None => C::Other,
                }),
            )
            .collect()
    }

    /// [`Self::sole_emitted_method`] with the slots no access and no variable names closed, as
    /// kotlinc's node has them: its codegen never enters a local nothing stores, where krusty's
    /// emission reserves one (a diverging `try`'s result) that only the class writer's last pass
    /// closes. The inliner hands slots out in the order the body names them, so a gap would place
    /// the body's later locals differently.
    pub(crate) fn sole_emitted_method_for_inlining(&mut self) -> Option<EmittedMethod> {
        let emitted = self.sole_emitted_method()?;
        let access = self.methods.first()?.access;
        let mut node =
            MethodNode::read(access, &emitted.name, &emitted.descriptor, &emitted.body).ok()?;
        let receiver = u16::from(access & 0x0008 == 0);
        let parameters = descriptors::argument_types(&emitted.descriptor)?
            .into_iter()
            .map(|argument| descriptors::size(argument) as u16)
            .sum::<u16>()
            + receiver;
        if !local_slots::compact(&mut node, &(0..parameters).collect()) {
            return Some(emitted);
        }
        let assembled = node.assemble(self).ok()?;
        let body = MethodCode {
            max_locals: assembled.max_locals,
            code: assembled.code,
            source_cp: self.reader_pool().into(),
            handlers: assembled
                .exception_table
                .iter()
                .map(|&(start_pc, end_pc, handler_pc, catch_type)| ExcEntry {
                    start_pc,
                    end_pc,
                    handler_pc,
                    catch_type,
                })
                .collect(),
            locals: assembled
                .local_variables
                .into_iter()
                .map(|local| MethodLocal {
                    start_pc: local.start_pc,
                    length: local.length,
                    slot: local.slot,
                    name: local.name,
                    descriptor: local.desc,
                })
                .collect(),
            lines: assembled.line_numbers,
            ..emitted.body
        };
        Some(EmittedMethod { body, ..emitted })
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
