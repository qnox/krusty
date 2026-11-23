//! Which calls of `kotlin.Any`'s members could reach a slot with no body.
//!
//! Every object answers `equals`, `hashCode` and `toString` through its table, and most tables
//! hold a body for each (see `super::super::super::classes` and `crate::runtime`). A few
//! still hold the trap of a member with no body: `toString` of a function value whose class
//! kotlinc-wasm does not name for its shape (`crate::function_value_names`), a lambda converted to
//! a `fun interface` or a reference to a property; and `toString` of a class the compiler
//! generated, which Kotlin gives no source name the default prints. Lowering declines a call that
//! could reach one rather than emit a trap the program would run into.
//!
//! A value statically of a class of this file is an instance of that class's hierarchy; one of a
//! `fun interface` of this file can also be a function value converted to it. Any other reference
//! (`Any`, a type parameter, an interface declared elsewhere) is erased: it can be an instance of
//! every class here, or one of the runtime's objects this file creates, including a runtime
//! exception this file constructs or catches. A `KType` value is narrower: the runtime's own
//! object, or a class implementing `KType`. Either can also be an object another file of the
//! module creates; the file records such a call (`super::super::super::any_member_reach`) for the
//! module to weigh against the other files.

use krusty::backend::class_tables::AnyMember;
use krusty::ir::{ClassId, IrExpr, IrFile};
use krusty::types::{Ty, TyKind};

use super::super::super::super::boxes::is_boxed_scalar;
use super::super::super::super::encode::ValType;
use super::super::super::super::function_value_names::{
    is_reflective_property_reference, named_shape, sam_class_name,
};
use super::super::super::any_member_reach::Receiver;
use super::{carrier, Body, Unsupported};

/// The first object whose existing reach predicate finds an unimplemented slot.
#[derive(Clone, Copy, Debug)]
pub(in super::super) enum UnrealizedMember {
    FunctionValue(u32),
    Class(ClassId),
    RuntimeException,
}

impl UnrealizedMember {
    pub(in super::super) fn describe(self, ir: &IrFile) -> String {
        match self {
            Self::FunctionValue(expression) => match ir.expr(expression) {
                IrExpr::Lambda {
                    sam: Some(target), ..
                } => format!(
                    "a lambda converted to `{}`",
                    target.classifier.render().replace('/', "."),
                ),
                IrExpr::Lambda { impl_fn, .. } => format!(
                    "a lambda implemented by {} with {}",
                    implementation_name(ir, *impl_fn),
                    checked_shape(ir.checked_type(expression)),
                ),
                IrExpr::CallableReference(reference) => {
                    if matches!(
                        reference.target,
                        krusty::ir::IrCallableReferenceTarget::Property(_)
                    ) {
                        "a property reference".to_owned()
                    } else {
                        format!(
                            "a callable reference implemented by {} with {}",
                            implementation_name(ir, reference.adapter),
                            checked_shape(Some(reference.function_type)),
                        )
                    }
                }
                _ => unreachable!("an unprinted value is a lambda or callable reference"),
            },
            Self::Class(class) => {
                let declaration = &ir.classes[class as usize];
                let kind = if declaration.annotation_impl_of.is_some() {
                    "an annotation implementation"
                } else if declaration.is_anonymous_object {
                    "an anonymous object"
                } else if declaration.is_local_class {
                    "a local class"
                } else if declaration.is_source_declared {
                    "a source class"
                } else {
                    "a generated class"
                };
                let naming =
                    if krusty::backend::class_names::source_class_names(ir, class).is_some() {
                        "with a source name"
                    } else {
                        "without a source name"
                    };
                format!(
                    "{kind} `{}` {naming}",
                    declaration.fq_name.render().replace('/', ".")
                )
            }
            Self::RuntimeException => "a runtime exception table".to_owned(),
        }
    }
}

/// Present the implementation's recorded name, without reconstructing a declaration identity.
fn implementation_name(ir: &IrFile, function: krusty::ir::FunId) -> String {
    ir.functions.get(function as usize).map_or_else(
        || "a missing implementation".to_owned(),
        |declaration| format!("`{}`", declaration.name),
    )
}

/// Describe the checked shape without exposing opaque type-parameter identities in a diagnostic.
fn checked_shape(ty: Option<Ty>) -> String {
    match ty.map(Ty::kind) {
        None => "no checked type".to_owned(),
        Some(TyKind::Nullable(inner)) => format!("nullable {}", checked_shape(Some(inner))),
        Some(TyKind::DefinitelyNotNull(inner)) => {
            format!("definitely non-null {}", checked_shape(Some(inner)))
        }
        Some(TyKind::TyParam(_, _)) => "a checked type parameter".to_owned(),
        Some(TyKind::Obj(classifier, _)) => {
            format!(
                "checked classifier `{}`",
                classifier.render().replace('/', ".")
            )
        }
        Some(TyKind::Fun(signature)) => format!(
            "a checked {}function of arity {}",
            if signature.suspend { "suspend " } else { "" },
            signature.params.len(),
        ),
        _ => "a checked non-function type".to_owned(),
    }
}

impl Body<'_> {
    /// Whether a call through `member`'s slot on a value statically of type `ty` could reach a
    /// slot with no body.
    pub(in super::super) fn may_reach_unrealized(
        &self,
        ty: Ty,
        member: AnyMember,
    ) -> Result<bool, Unsupported> {
        Ok(self.first_unrealized_member(ty, member)?.is_some())
    }

    /// The same decision as `may_reach_unrealized`, retaining its first witness for diagnostics.
    pub(in super::super) fn first_unrealized_member(
        &self,
        ty: Ty,
        member: AnyMember,
    ) -> Result<Option<UnrealizedMember>, Unsupported> {
        if let Some(class) = self.class_of_type(ty) {
            let declaration = &self.ir.classes[class as usize];
            if declaration.is_interface {
                if declaration.is_fun_interface {
                    // A function value converted to a `fun interface` is an instance of it too.
                    self.record_call(member, ty, Receiver::Erased);
                    let unprinted = (member == AnyMember::ToString)
                        .then(|| first_unprinted_value(self.ir))
                        .flatten();
                    if let Some(expression) = unprinted {
                        return Ok(Some(UnrealizedMember::FunctionValue(expression)));
                    }
                } else {
                    // A plain interface is only implemented by classes, including another file's.
                    self.record_call(member, ty, Receiver::Classes);
                }
            } else if declaration.is_open || declaration.is_abstract || declaration.is_sealed {
                self.record_call(member, ty, Receiver::Classes);
            }
            return Ok(self
                .reaches_unrealized(class, member)?
                .map(UnrealizedMember::Class));
        }
        let value = ty.non_null();
        if value == Ty::String {
            // A string answers every member itself.
            return Ok(None);
        }
        if is_boxed_scalar(value) {
            // A scalar read as a reference is its box, which answers every member.
            return Ok(None);
        }
        if matches!(value.kind(), TyKind::Fun(_)) {
            // Only a function value is of a function type. Most print their class name, but one
            // of this file or another can be a reference to a property.
            if member != AnyMember::ToString {
                return Ok(None);
            }
            self.record_call(member, ty, Receiver::Erased);
            return Ok(first_unprinted_value(self.ir).map(UnrealizedMember::FunctionValue));
        }
        if !matches!(carrier(value)?, Some(ValType::Ref { .. })) {
            return Ok(None);
        }
        if value.obj_internal() == Some(super::super::super::super::type_objects::ktype()) {
            // The runtime object answers every member. Another file's class can implement
            // `KType`; a box or a function value cannot, so this is not an erased call.
            self.record_call(member, ty, Receiver::Classes);
            return Ok(self
                .ktype_unrealized_class(member)?
                .map(UnrealizedMember::Class));
        }
        self.record_call(member, ty, Receiver::Erased);
        let class_elsewhere = self.classifier_declared_elsewhere(value);
        let unprinted = (member == AnyMember::ToString && class_elsewhere.is_none())
            .then(|| first_unprinted_value(self.ir))
            .flatten();
        if let Some(expression) = unprinted {
            return Ok(Some(UnrealizedMember::FunctionValue(expression)));
        }
        // A runtime exception this file builds or catches is not a class of the file.
        // Its table installs `equals`, `hashCode` and `Throwable.toString`.
        if self.holds_runtime_exception_objects() && runtime_exception_lacks_body(member) {
            return Ok(Some(UnrealizedMember::RuntimeException));
        }
        let Some(classes) = self.classes else {
            return Ok(None);
        };
        Ok((0..self.ir.classes.len() as ClassId)
            .find(|&class| {
                classes.has_bodiless_instances(class, member)
                    && class_elsewhere.is_none_or(|ancestor| {
                        self.classifiers
                            .is_subclassifier(self.ir.classes[class as usize].fq_name, ancestor)
                    })
            })
            .map(UnrealizedMember::Class))
    }

    /// Whether a call through `member`'s slot on an instance of `class`, or of a class extending
    /// or implementing it, could reach a slot with no body.
    fn reaches_unrealized(
        &self,
        class: ClassId,
        member: AnyMember,
    ) -> Result<Option<ClassId>, Unsupported> {
        let classes = self.file_classes()?;
        Ok(self
            .ir
            .classes
            .iter()
            .enumerate()
            .find_map(|(id, candidate)| {
                let id = id as ClassId;
                if candidate.is_interface {
                    return None;
                }
                let mut ancestor = Some(id);
                let mut related = classes.tables.interfaces[id as usize].contains(&class);
                while let Some(current) = ancestor {
                    related |= current == class;
                    ancestor = classes.tables.table(current).superclass;
                }
                (related && classes.has_bodiless_instances(id, member)).then_some(id)
            }))
    }

    /// The classifier of `value` when it is declared elsewhere and no value whose `toString` has
    /// no body can be an instance of it, other than a class of this file implementing it. A
    /// function value is an instance of `Any`, of `kotlin.Function`'s hierarchy and of the
    /// `fun interface` it is converted to, so an interface counts only in a file converting none.
    fn classifier_declared_elsewhere(&self, value: Ty) -> Option<krusty::types::TypeName> {
        let internal = value.obj_internal()?;
        if internal == krusty::types::wk::any() {
            return None;
        }
        let fact = self.classifiers.classifier(internal)?;
        let holds_function_values = matches!(
            fact.kind,
            krusty::libraries::TypeKind::Interface | krusty::libraries::TypeKind::Annotation
        ) && (self
            .classifiers
            .is_subclassifier(krusty::types::wk::function_root(), internal)
            || converts_function_values(self.ir));
        (!holds_function_values).then_some(internal)
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

/// Whether `ir` converts a function value to a `fun interface`.
fn converts_function_values(ir: &IrFile) -> bool {
    ir.classes.iter().any(|class| class.sam_wrapper.is_some())
        || ir
            .exprs
            .iter()
            .any(|node| matches!(node, IrExpr::Lambda { sam: Some(_), .. }))
}

/// Whether a reference of `ir` can hold a runtime object whose `toString` has no body: a function
/// value kotlinc-wasm does not name for its shape or its `fun interface`, which only a lambda or a callable reference the
/// file emits creates.
pub(in super::super) fn holds_unprinted_values(ir: &IrFile) -> bool {
    first_unprinted_value(ir).is_some()
}

fn first_unprinted_value(ir: &IrFile) -> Option<u32> {
    krusty::ir::emitted_body_roots(ir)
        .into_iter()
        .flat_map(|root| krusty::ir::value_namespace_expressions(ir, root))
        .find(|&expression| {
            let node = ir.expr(expression);
            matches!(node, IrExpr::Lambda { .. } | IrExpr::CallableReference(_))
                && !matches!(node, IrExpr::CallableReference(reference)
                    if is_reflective_property_reference(reference))
                && named_shape(node, ir.checked_type(expression)).is_none()
                && sam_class_name(ir, expression).is_none()
        })
}
