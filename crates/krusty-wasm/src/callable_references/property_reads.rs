//! `KProperty0.get()`, `KProperty1.get(receiver)` and `KProperty2.get(dispatch, extension)` of a
//! property-reference object.
//!
//! A property reference is a function value whose entry reads the property (`crate::function_values`),
//! and `get` reads it the same way, so each reference class implements the library member with a
//! function forwarding its operands to the entry. A program can call `get` through any type the
//! reference wears, a variable or a parameter rather than the written reference, so every
//! reference class of that arity implements it.
//!
//! The member is named by its KLIB identity, which only a call through it supplies. A class created
//! before that call is remembered with its entry and implements the member once the call names it.
//! A class created after implements it right away.

use std::mem;

use krusty::fx_hash::HashMap;

use crate::encode::{Code, Function, Module, ValType};
use crate::interface_dispatch::{ClassRef, MemberRef};
use crate::objects::REFERENCE;
use crate::runtime::Runtime;

/// The `get` member of each arity a call has named, and the reference classes, with their entries,
/// that wait for the member of theirs.
#[derive(Default)]
pub(crate) struct PropertyReads {
    members: HashMap<usize, MemberRef>,
    pending: Vec<PendingRead>,
}

struct PendingRead {
    class: ClassRef,
    arity: usize,
    entry: u32,
}

impl Runtime {
    /// Record that the property-reference objects of `class`, whose function-value `entry` takes
    /// `arity` operands, answer `get` through that entry.
    pub(crate) fn property_reference_read(
        &mut self,
        module: &mut Module,
        class: ClassRef,
        arity: usize,
        entry: u32,
    ) -> Result<(), String> {
        match self.callable_references.reads.members.get(&arity) {
            Some(&member) => self.implement_property_read(module, member, class, arity, entry),
            None => {
                self.callable_references.reads.pending.push(PendingRead {
                    class,
                    arity,
                    entry,
                });
                Ok(())
            }
        }
    }

    /// Record that `member`, which a call goes through with `arity` operands after the reference,
    /// is `get`, and implement it for every reference class of that arity recorded so far.
    pub(crate) fn property_reference_read_member(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        arity: usize,
    ) -> Result<(), String> {
        match self.callable_references.reads.members.get(&arity) {
            Some(&known) if known == member => return Ok(()),
            Some(_) => return Err(format!("a `get` of {arity} operands named by two members")),
            None => {
                self.callable_references.reads.members.insert(arity, member);
            }
        }
        let (ready, waiting) = mem::take(&mut self.callable_references.reads.pending)
            .into_iter()
            .partition::<Vec<_>, _>(|pending| pending.arity == arity);
        self.callable_references.reads.pending = waiting;
        for pending in ready {
            self.implement_property_read(module, member, pending.class, arity, pending.entry)?;
        }
        Ok(())
    }

    fn implement_property_read(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        class: ClassRef,
        arity: usize,
        entry: u32,
    ) -> Result<(), String> {
        // `get`'s receiver and result are type parameters, so its operands cross as the
        // references the entry takes and answers.
        let ty = self.objects.interfaces.ty(member);
        let expected: (Vec<ValType>, Vec<ValType>) = (vec![REFERENCE; arity + 1], vec![REFERENCE]);
        if module.func_signature(ty) != Some(expected) {
            return Err("a property-reference `get` whose operands are not references".to_string());
        }
        let mut code = Code::default();
        for operand in 0..=arity as u32 {
            code.local_get(operand);
        }
        code.call(entry);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        self.objects.interfaces.implement(class, member, function)
    }
}
