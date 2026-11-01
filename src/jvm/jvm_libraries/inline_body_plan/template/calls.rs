//! What a call in a decoded inline body means, decided by its exact JVM target alone.
//!
//! A target is either one of the JVM realization members kotlinc compiles Kotlin semantics into
//! (a primitive box or unbox, a value class's `box-impl`/`unbox-impl`, a function object's
//! `invoke`, a parameter null check, an inline marker), each matched by exact owner, name and
//! descriptor, or a member the provider normalizes to exactly one metadata declaration. Anything
//! else declines the template.

use super::super::{InlineDependency, JvmLibraries, MethodTarget};
use crate::jvm::inline_template::{InvokeKind, MethodRef};
use crate::libraries::{LibraryCallable, TemplateCallee, TemplateDecline};
use crate::types::{type_name, Ty, TypeName};

/// The meaning of one invoke.
pub(super) enum CallForm {
    /// kotlinc's inline marker: no semantics of its own.
    Marker,
    /// `Intrinsics.checkNotNullParameter(value, name)`.
    ParameterNullCheck,
    /// `valueOf` of the box of `primitive`.
    Box { primitive: Ty },
    /// The `primitive` value of a box (`Integer.intValue`, `Number.intValue`, …).
    Unbox { primitive: Ty },
    /// `box-impl` of the value class `class`.
    ValueClassBox { class: TypeName },
    /// `unbox-impl` of the value class `class`, whose underlying type is `underlying`.
    ValueClassUnbox { class: TypeName, underlying: Ty },
    /// `FunctionN.invoke` with `arity` arguments.
    FunctionInvoke { arity: usize },
    /// A declaration call returning `result` (`None` for `Unit` realized as `void`).
    Declared {
        callee: TemplateCallee,
        result: Option<Ty>,
    },
}

/// The JVM box class of each primitive, with its descriptor and unbox method.
const BOXES: [(&str, Ty, &str, &str); 8] = [
    ("java/lang/Boolean", Ty::Boolean, "Z", "booleanValue"),
    ("java/lang/Byte", Ty::Byte, "B", "byteValue"),
    ("java/lang/Character", Ty::Char, "C", "charValue"),
    ("java/lang/Short", Ty::Short, "S", "shortValue"),
    ("java/lang/Integer", Ty::Int, "I", "intValue"),
    ("java/lang/Long", Ty::Long, "J", "longValue"),
    ("java/lang/Float", Ty::Float, "F", "floatValue"),
    ("java/lang/Double", Ty::Double, "D", "doubleValue"),
];

const NUMBER: &str = "java/lang/Number";
const INTRINSICS: &str = "kotlin/jvm/internal/Intrinsics";
const INLINE_MARKER: &str = "kotlin/jvm/internal/InlineMarker";
const OBJECT: &str = "Ljava/lang/Object;";

/// The JVM class a value of `ty` is boxed into, when `ty` is a primitive.
pub(super) fn primitive_box(ty: Ty) -> Option<&'static str> {
    BOXES
        .iter()
        .find(|(_, primitive, _, _)| *primitive == ty)
        .map(|(owner, ..)| *owner)
}

fn call_decline(target: &MethodRef, reason: &'static str) -> TemplateDecline {
    TemplateDecline::Call {
        owner: target.owner.clone(),
        name: target.name.clone(),
        descriptor: target.descriptor.clone(),
        reason,
    }
}

/// The realization members, by exact target.
fn realization_form(
    kind: InvokeKind,
    target: &MethodRef,
) -> Option<Result<CallForm, TemplateDecline>> {
    let MethodRef {
        owner,
        name,
        descriptor,
        interface,
    } = target;
    let (owner, name, descriptor) = (owner.as_str(), name.as_str(), descriptor.as_str());
    if *interface {
        return function_invoke(kind, owner, name, descriptor).map(Ok);
    }
    match (kind, owner) {
        (InvokeKind::Static, INLINE_MARKER)
            if matches!(
                (name, descriptor),
                ("mark" | "finallyStart" | "finallyEnd", "(I)V")
                    | ("beforeInlineCall" | "afterInlineCall", "()V")
            ) =>
        {
            return Some(Ok(CallForm::Marker));
        }
        (InvokeKind::Static, INTRINSICS) => match (name, descriptor) {
            (
                "checkNotNullParameter" | "checkParameterIsNotNull",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            ) => return Some(Ok(CallForm::ParameterNullCheck)),
            ("reifiedOperationMarker", "(ILjava/lang/String;)V")
            | ("needClassReification", "()V") => return Some(Err(TemplateDecline::Reified)),
            _ => {}
        },
        _ => {}
    }
    for (box_class, primitive, primitive_descriptor, unbox) in BOXES {
        let boxed = kind == InvokeKind::Static
            && owner == box_class
            && name == "valueOf"
            && descriptor == format!("({primitive_descriptor})L{box_class};");
        if boxed {
            return Some(Ok(CallForm::Box { primitive }));
        }
        let numeric = !matches!(primitive_descriptor, "Z" | "C");
        let unboxed = kind == InvokeKind::Virtual
            && (owner == box_class || (numeric && owner == NUMBER))
            && name == unbox
            && descriptor == format!("(){primitive_descriptor}");
        if unboxed {
            return Some(Ok(CallForm::Unbox { primitive }));
        }
    }
    None
}

/// `kotlin/jvm/functions/FunctionN.invoke` taking and returning `Object`s.
fn function_invoke(
    kind: InvokeKind,
    owner: &str,
    name: &str,
    descriptor: &str,
) -> Option<CallForm> {
    let arguments = descriptor
        .strip_prefix('(')?
        .strip_suffix(&format!("){OBJECT}"))?;
    let arity = arguments.len() / OBJECT.len();
    let exact = kind == InvokeKind::Interface
        && name == "invoke"
        && arguments == OBJECT.repeat(arity)
        && owner == format!("kotlin/jvm/functions/Function{arity}");
    exact.then_some(CallForm::FunctionInvoke { arity })
}

fn method_target(target: &MethodRef) -> MethodTarget<'_> {
    (
        target.owner.as_str(),
        target.name.as_str(),
        target.descriptor.as_str(),
        target.interface,
    )
}

/// Whether templates may call a declaration of these semantic parameter and result types: not
/// a generic one, whose types a template could not substitute.
fn monomorphic(params: &[Ty], ret: Ty) -> bool {
    !params.iter().chain([&ret]).any(|ty| ty.mentions_ty_param())
}

/// The value a call leaves, `None` when its realization returns `void`.
fn value_result(ret: Ty, physical_ret: Ty) -> Option<Ty> {
    (physical_ret != Ty::Unit).then_some(ret)
}

impl JvmLibraries {
    /// The meaning of an invoke of `target` by `kind` in a template body.
    pub(super) fn template_call(
        &self,
        kind: InvokeKind,
        target: &MethodRef,
    ) -> Result<CallForm, TemplateDecline> {
        if let Some(form) = realization_form(kind, target) {
            return form;
        }
        if let Some(form) = self.value_class_form(kind, target) {
            return Ok(form);
        }
        let callee = match kind {
            InvokeKind::Static => self.static_template_callee(target)?,
            InvokeKind::Virtual | InvokeKind::Interface => TemplateCallee::Member(Box::new(
                self.inline_plan_member(method_target(target))
                    .ok_or_else(|| call_decline(target, "no single member declaration"))?,
            )),
            InvokeKind::Special if target.name == "<init>" => {
                let (_, constructor) = self
                    .inline_plan_constructor(method_target(target))
                    .ok_or_else(|| call_decline(target, "no single constructor declaration"))?;
                TemplateCallee::Constructor(Box::new(constructor))
            }
            InvokeKind::Special => return Err(call_decline(target, "a non-virtual member call")),
        };
        let (inline, params, result) = match &callee {
            TemplateCallee::Function(function) => (
                function.inline.can_inline(),
                function.params.as_slice(),
                value_result(function.ret, function.physical_ret),
            ),
            TemplateCallee::Member(member) => (
                member.inline.can_inline(),
                member.params.as_slice(),
                value_result(member.ret, member.physical_ret),
            ),
            TemplateCallee::Constructor(constructor) => (
                constructor.inline.can_inline(),
                constructor.params.as_slice(),
                Some(Ty::obj_name(constructor.owner.ok_or_else(|| {
                    call_decline(target, "a constructor without its class")
                })?)),
            ),
        };
        if inline {
            return Err(call_decline(target, "an inline callee"));
        }
        if !monomorphic(params, result.unwrap_or(Ty::Unit)) {
            return Err(call_decline(target, "a generic callee"));
        }
        Ok(CallForm::Declared { callee, result })
    }

    /// A static target: exactly one of the top-level functions, extension functions and
    /// constructors (a value class's `constructor-impl`) the provider normalizes it to.
    fn static_template_callee(
        &self,
        target: &MethodRef,
    ) -> Result<TemplateCallee, TemplateDecline> {
        let exact = method_target(target);
        let mut candidates: Vec<TemplateCallee> = Vec::new();
        // A normalizer that finds the physical method but not a declaration of its kind rejects
        // it; the target is then not that kind's candidate.
        let mut rejected = false;
        for dependency in [
            self.inline_plan_static_top_level(exact),
            self.inline_plan_static_extension(exact),
        ] {
            match dependency {
                InlineDependency::Found(function) => {
                    candidates.push(TemplateCallee::Function(Box::new(function)))
                }
                InlineDependency::Rejected => rejected = true,
                InlineDependency::Unavailable => {}
            }
        }
        if let Some((_, constructor)) = self.inline_plan_constructor(exact) {
            candidates.push(TemplateCallee::Constructor(Box::new(constructor)));
        }
        match <[TemplateCallee; 1]>::try_from(candidates) {
            Ok([callee]) => Ok(callee),
            Err(candidates) if candidates.is_empty() && rejected => {
                Err(call_decline(target, "a declaration the provider rejects"))
            }
            Err(candidates) if candidates.is_empty() => Err(call_decline(target, "no declaration")),
            Err(_) => Err(call_decline(target, "more than one declaration")),
        }
    }

    /// `box-impl`/`unbox-impl` of a value class the provider's metadata declares, by exact
    /// descriptor over its underlying type.
    fn value_class_form(&self, kind: InvokeKind, target: &MethodRef) -> Option<CallForm> {
        if target.interface {
            return None;
        }
        let class = type_name(&target.owner);
        let underlying = self.metadata_value_class_underlying(class)?;
        let boxed = Ty::obj_name(class);
        match (kind, target.name.as_str()) {
            (InvokeKind::Static, "box-impl")
                if target.descriptor
                    == crate::jvm::names::method_descriptor(&[underlying], boxed) =>
            {
                Some(CallForm::ValueClassBox { class })
            }
            (InvokeKind::Virtual, "unbox-impl")
                if target.descriptor == crate::jvm::names::method_descriptor(&[], underlying) =>
            {
                Some(CallForm::ValueClassUnbox { class, underlying })
            }
            _ => None,
        }
    }
}

/// Whether `callable`'s parameter `parameter` is a non-suspend function of `arity` parameters
/// (an extension function type's receiver counted), the shape `FunctionN.invoke` calls.
pub(super) fn lambda_parameter(
    callable: &LibraryCallable,
    parameter: usize,
    arity: usize,
) -> Option<Ty> {
    let ty = *callable.params.get(parameter)?;
    let crate::types::TyKind::Fun(signature) = ty.kind() else {
        return None;
    };
    (!signature.suspend && signature.params.len() == arity).then_some(signature.ret)
}
