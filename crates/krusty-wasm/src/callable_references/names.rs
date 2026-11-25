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

use crate::encode::{Code, Function, HeapType, Module};
use crate::interface_dispatch::{ClassRef, MemberRef};
use crate::runtime::Runtime;

/// The reference classes and the names they answer, and the member they answer it through once a
/// call has named it.
#[derive(Default)]
pub(crate) struct ReferenceNames {
    member: Option<MemberRef>,
    pending: Vec<(ClassRef, Name)>,
}

/// Where a reference class's answer to `KCallable.name` comes from.
enum Name {
    /// One declaration's name, the same for every object of the class.
    Constant(Vec<u16>),
    /// The `String` field of the object's own struct, for a class whose objects are properties
    /// of different declarations.
    Field { structure: u32, field: u32 },
}

impl Runtime {
    /// Record that the reference objects of `class` answer `KCallable.name` with `name`.
    pub(crate) fn reference_name(
        &mut self,
        module: &mut Module,
        class: ClassRef,
        name: &str,
    ) -> Result<(), String> {
        self.record_reference_name(
            module,
            class,
            Name::Constant(name.encode_utf16().collect::<Vec<_>>()),
        )
    }

    /// Record that the objects of `class`, whose struct is `structure`, answer `KCallable.name`
    /// with their own `String` field `field`.
    pub(crate) fn reference_name_field(
        &mut self,
        module: &mut Module,
        class: ClassRef,
        structure: u32,
        field: u32,
    ) -> Result<(), String> {
        self.record_reference_name(module, class, Name::Field { structure, field })
    }

    fn record_reference_name(
        &mut self,
        module: &mut Module,
        class: ClassRef,
        name: Name,
    ) -> Result<(), String> {
        match self.callable_references.names.member {
            Some(member) => self.implement_reference_name(module, member, class, &name),
            None => {
                self.callable_references.names.pending.push((class, name));
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
        for (class, name) in mem::take(&mut self.callable_references.names.pending) {
            self.implement_reference_name(module, member, class, &name)?;
        }
        Ok(())
    }

    fn implement_reference_name(
        &mut self,
        module: &mut Module,
        member: MemberRef,
        class: ClassRef,
        name: &Name,
    ) -> Result<(), String> {
        let mut code = Code::default();
        match name {
            Name::Constant(units) => self.string_constant(module, &mut code, units),
            Name::Field { structure, field } => {
                code.local_get(0)
                    .ref_cast(false, HeapType::Concrete(*structure))
                    .struct_get(*structure, *field);
            }
        }
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
