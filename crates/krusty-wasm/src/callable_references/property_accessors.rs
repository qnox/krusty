//! `KProperty0.get()`, `KProperty1.get(receiver)` and `KProperty2.get(dispatch, extension)` of a
//! property-reference object, and `set` of a mutable one.
//!
//! A property reference is a function value whose entry reads the property
//! (`crate::function_values`), and `get` reads it the same way, so each reference class implements
//! the library member with a function forwarding its operands to the entry. A mutable reference
//! has a second entry over the same captures that writes the property, which `set` forwards to. A
//! program can call either member through any type the reference wears, a variable or a parameter
//! rather than the written reference, so every reference class of that shape implements it.
//!
//! The member is named by its KLIB identity, which only a call through it supplies. A class created
//! before that call is remembered with its entry and implements the member once the call names it.
//! A class created after implements it right away.

use std::mem;

use krusty::fx_hash::HashMap;

use crate::encode::{Code, Function, Module};
use crate::interface_dispatch::{ClassRef, MemberRef};
use crate::objects::REFERENCE;
use crate::runtime::Runtime;

/// Which accessor of the property a member is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PropertyAccess {
    /// `get`, whose operands are the reference's unbound receivers.
    Read,
    /// `set`, whose operands are the reference's unbound receivers, then the written value.
    Write,
}

/// One accessor shape: the access and how many operands its entry takes after the reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct AccessorShape {
    pub(crate) access: PropertyAccess,
    pub(crate) operands: usize,
}

/// The accessor members a call has named, by shape, and the reference classes, with their
/// entries, that wait for the member of theirs.
#[derive(Default)]
pub(crate) struct PropertyAccessors {
    members: HashMap<AccessorShape, MemberRef>,
    pending: Vec<PendingAccessor>,
}

struct PendingAccessor {
    class: ClassRef,
    shape: AccessorShape,
    entry: u32,
}

impl Runtime {
    /// Record that the property-reference objects of `class` answer the accessor of `shape`
    /// through `entry`, a function-value entry taking the shape's operands after the reference.
    pub(crate) fn property_reference_accessor(
        &mut self,
        module: &mut Module,
        class: ClassRef,
        shape: AccessorShape,
        entry: u32,
    ) -> Result<(), String> {
        let accessors = &mut self.callable_references.accessors;
        match accessors.members.get(&shape) {
            Some(&member) => self.implement_property_accessor(module, member, class, shape, entry),
            None => {
                accessors.pending.push(PendingAccessor {
                    class,
                    shape,
                    entry,
                });
                Ok(())
            }
        }
    }

    /// Record that `member`, which a call goes through as the accessor of `shape`, is that
    /// accessor, and implement it for every reference class of that shape recorded so far.
    pub(crate) fn property_reference_accessor_member(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        shape: AccessorShape,
    ) -> Result<(), String> {
        let accessors = &mut self.callable_references.accessors;
        match accessors.members.get(&shape) {
            Some(&known) if known == member => return Ok(()),
            Some(_) => {
                return Err(format!(
                    "a property-reference {shape:?} named by two members"
                ))
            }
            None => {
                accessors.members.insert(shape, member);
            }
        }
        let (ready, waiting) = mem::take(&mut accessors.pending)
            .into_iter()
            .partition::<Vec<_>, _>(|pending| pending.shape == shape);
        accessors.pending = waiting;
        for pending in ready {
            self.implement_property_accessor(module, member, pending.class, shape, pending.entry)?;
        }
        Ok(())
    }

    fn implement_property_accessor(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        class: ClassRef,
        shape: AccessorShape,
        entry: u32,
    ) -> Result<(), String> {
        // The accessor's receivers, value and result are type parameters, so its operands cross
        // as the references the entry takes. The entry answers a reference, which the `Unit`
        // result of `set` drops.
        let ty = self.objects.interfaces.ty(member);
        let operands = vec![REFERENCE; shape.operands + 1];
        let drop = match module.func_signature(ty) {
            Some((params, results)) if params == operands => match results.as_slice() {
                [result] if *result == REFERENCE => false,
                [] if shape.access == PropertyAccess::Write => true,
                _ => {
                    return Err(format!(
                        "a property-reference {shape:?} with another result"
                    ))
                }
            },
            _ => {
                return Err(format!(
                    "a property-reference {shape:?} whose operands are not references"
                ));
            }
        };
        let mut code = Code::default();
        for operand in 0..operands.len() as u32 {
            code.local_get(operand);
        }
        code.call(entry);
        if drop {
            code.drop_();
        }
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
