//! `KCallable.name` of a callable-reference object.
//!
//! A reference is a `KCallable`, so a program can read its `name` through any type it wears,
//! including a variable or a parameter rather than the written reference. Each reference object's
//! class therefore implements the library member, with a function answering the referenced
//! declaration's name (`krusty::backend::reference_names`).
//!
//! The member is named by its KLIB identity, which only a call through it supplies. A class
//! created before that call is remembered with its name and implements the member once the call
//! names it. A class created after implements it right away.

use std::mem;

use crate::encode::{Code, Function, Module};
use crate::interface_dispatch::{ClassRef, MemberRef};
use crate::runtime::Runtime;

/// The reference classes and the names they answer, and the member they answer it through once a
/// call has named it.
#[derive(Default)]
pub(crate) struct ReferenceNames {
    member: Option<MemberRef>,
    pending: Vec<(ClassRef, Vec<u16>)>,
}

impl Runtime {
    /// Record that the reference objects of `class` answer `KCallable.name` with `name`.
    pub(crate) fn reference_name(
        &mut self,
        module: &mut Module,
        class: ClassRef,
        name: &str,
    ) -> Result<(), String> {
        let units = name.encode_utf16().collect::<Vec<_>>();
        match self.callable_references.names.member {
            Some(member) => self.implement_reference_name(module, member, class, &units),
            None => {
                self.callable_references.names.pending.push((class, units));
                Ok(())
            }
        }
    }

    /// Record that `member` is `KCallable.name`, which a call goes through, and implement it for
    /// every reference class recorded so far.
    pub(crate) fn reference_name_member(
        &mut self,
        module: &mut Module,
        member: MemberRef,
    ) -> Result<(), String> {
        match self.callable_references.names.member {
            Some(known) if known == member => return Ok(()),
            Some(_) => return Err("`KCallable.name` named by two members".to_string()),
            None => self.callable_references.names.member = Some(member),
        }
        for (class, units) in mem::take(&mut self.callable_references.names.pending) {
            self.implement_reference_name(module, member, class, &units)?;
        }
        Ok(())
    }

    fn implement_reference_name(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        class: ClassRef,
        units: &[u16],
    ) -> Result<(), String> {
        let mut code = Code::default();
        self.string_constant(module, &mut code, units);
        let function = module.declare();
        let ty = self.objects.interfaces.ty(member);
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
