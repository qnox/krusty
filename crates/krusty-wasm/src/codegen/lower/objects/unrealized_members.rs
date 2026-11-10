//! Which calls of `kotlin.Any`'s members could reach a slot with no body.
//!
//! Every object answers `equals`, `hashCode` and `toString` through its table, and most tables
//! hold a body for each (see `super::super::super::classes` and `crate::runtime`). A few
//! still hold the trap of a member with no body: `toString` of a floating box, for which no printer
//! writes Kotlin's form; `toString` of a function value; and `toString` of a class the compiler
//! generated, which Kotlin gives no source name the default prints. Lowering declines a call that
//! could reach one rather than emit a trap the program would run into.
//!
//! A value statically of a class of this file is an instance of that class's hierarchy; one of an
//! interface of this file can also be a function value converted to it. Any other reference
//! (`Any`, a type parameter, an interface declared elsewhere) is erased: it can be an instance of
//! every class here, or one of the runtime's objects this file creates, including a runtime
//! exception this file constructs or catches. A `KType` value is narrower: the runtime's own
//! object, or a class implementing `KType`. Either can also be an object another file of the
//! module creates; the file records such a call (`super::super::super::any_member_reach`) for the
//! module to weigh against the other files.

use krusty::backend::class_tables::AnyMember;
use krusty::ir::{ClassId, IrExpr, IrFile};
use krusty::types::{Ty, TyKind};

use super::super::super::super::boxes::box_prints;
use super::super::super::super::encode::ValType;
use super::super::super::any_member_reach::Receiver;
use super::{carrier, Body, Unsupported};

impl Body<'_> {
    /// Whether a call through `member`'s slot on a value statically of type `ty` could reach a
    /// slot with no body.
    pub(in super::super) fn may_reach_unrealized(
        &self,
        ty: Ty,
        member: AnyMember,
    ) -> Result<bool, Unsupported> {
        if let Some(class) = self.class_of_type(ty) {
            let declaration = &self.ir.classes[class as usize];
            if declaration.is_interface {
                // A function value converted to a `fun interface` is an instance of it too.
                self.record_call(member, ty, Receiver::Erased);
                if member == AnyMember::ToString && holds_unprinted_values(self.ir) {
                    return Ok(true);
                }
            } else if declaration.is_open || declaration.is_abstract || declaration.is_sealed {
                self.record_call(member, ty, Receiver::Classes);
            }
            return self.reaches_unrealized(class, member);
        }
        let value = ty.non_null();
        if let Some(prints) = box_prints(value) {
            // A scalar read as a reference is its box.
            return Ok(member == AnyMember::ToString && !prints);
        }
        if matches!(value.kind(), TyKind::Fun(_)) {
            return Ok(member == AnyMember::ToString);
        }
        if !matches!(carrier(value)?, Some(ValType::Ref { .. })) {
            return Ok(false);
        }
        if value.obj_internal() == Some(super::super::super::super::type_objects::ktype()) {
            // The runtime object answers every member. Another file's class can implement
            // `KType`; a floating box or a function value cannot, so this is not an erased call.
            self.record_call(member, ty, Receiver::Classes);
            return self.ktype_reaches_unrealized(member);
        }
        self.record_call(member, ty, Receiver::Erased);
        if member == AnyMember::ToString && holds_unprinted_values(self.ir) {
            return Ok(true);
        }
        // A runtime exception this file builds or catches is not a class of the file.
        // Its table installs `equals`, `hashCode` and `Throwable.toString`.
        if self.holds_runtime_exception_objects() && runtime_exception_lacks_body(member) {
            return Ok(true);
        }
        let Some(classes) = self.classes else {
            return Ok(false);
        };
        Ok((0..self.ir.classes.len()).any(|class| !classes.realizes(class as ClassId, member)))
    }

    /// Whether a call through `member`'s slot on an instance of `class`, or of a class extending
    /// or implementing it, could reach a slot with no body.
    fn reaches_unrealized(&self, class: ClassId, member: AnyMember) -> Result<bool, Unsupported> {
        let classes = self.file_classes()?;
        Ok(self.ir.classes.iter().enumerate().any(|(id, candidate)| {
            let id = id as ClassId;
            if candidate.is_interface {
                return false;
            }
            let mut ancestor = Some(id);
            let mut related = classes.tables.interfaces[id as usize].contains(&class);
            while let Some(current) = ancestor {
                related |= current == class;
                ancestor = classes.tables.table(current).superclass;
            }
            related && !classes.realizes(id, member)
        }))
    }

    /// Whether this file constructs a runtime exception or binds one in a catch. Those objects
    /// are not classes of the file, so the class walk above does not see their tables.
    fn holds_runtime_exception_objects(&self) -> bool {
        self.ir.exprs.iter().any(|node| match node {
            IrExpr::Try { catches, .. } => !catches.is_empty(),
            IrExpr::New { internal, .. } => self
                .classifiers
                .classifier(*internal)
                .is_some_and(|fact| fact.runtime_exception.is_some()),
            _ => false,
        })
    }

    /// Record, for the module, a call through `member`'s slot on a `ty` that can receive
    /// `receiver` from another file.
    fn record_call(&self, member: AnyMember, ty: Ty, receiver: Receiver) {
        self.reach.borrow_mut().call(member, receiver, || {
            let name = match member {
                AnyMember::Equals => "equals",
                AnyMember::HashCode => "hashCode",
                AnyMember::ToString => "toString",
            };
            format!("a `{name}` of `{}`", super::super::describe(ty))
        });
    }
}

/// Whether a runtime exception object's table has no body for `member`. The runtime installs
/// `equals`, `hashCode` and `Throwable.toString` on every such table.
fn runtime_exception_lacks_body(member: AnyMember) -> bool {
    match member {
        AnyMember::Equals | AnyMember::HashCode | AnyMember::ToString => false,
    }
}

/// Whether a reference of `ir` can hold a runtime object whose `toString` has no body: a floating
/// box, which only a floating value the file emits becomes, or a function value, which only a
/// lambda or a callable reference the file emits creates.
pub(in super::super) fn holds_unprinted_values(ir: &IrFile) -> bool {
    krusty::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| krusty::ir::value_namespace_expressions(ir, root))
        .any(|expression| {
            matches!(
                ir.expr(expression),
                IrExpr::Lambda { .. } | IrExpr::CallableReference(_)
            ) || ir
                .checked_type(expression)
                .and_then(|ty| box_prints(ty.non_null()))
                == Some(false)
        })
}
