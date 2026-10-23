//! Source-like type names used only at the diagnostic boundary.
//!
//! The semantic type model remains identity based. This module owns the extra spelling rules
//! needed by variance diagnostics: complete function-type shapes, nullability markers before a
//! type parameter's owner wording, and Java collection mutability flexibility.

use super::{type_parameter_source_name, FnSig, Ty, TypeName};
use crate::types::TyKind;

pub(super) fn function_type(
    signature: &FnSig,
    context: &[Ty],
    type_parameter: &dyn Fn(&str) -> String,
    classifier: &dyn Fn(TypeName) -> Option<String>,
    platform_upper_bound: &dyn Fn(Ty) -> Ty,
) -> String {
    let render = |ty: Ty| {
        ty.source_name_with_classifier_in(context, type_parameter, classifier, platform_upper_bound)
    };
    let join = |types: &[Ty]| {
        types
            .iter()
            .copied()
            .map(render)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let contexts = signature.context_count.min(signature.params.len());
    let receiver = signature
        .has_receiver
        .then(|| signature.params.get(contexts).copied())
        .flatten();
    let values = &signature.params[contexts + usize::from(receiver.is_some())..];
    let suspend = if signature.suspend { "suspend " } else { "" };
    let context = if contexts == 0 {
        String::new()
    } else {
        format!("context({}) ", join(&signature.params[..contexts]))
    };
    // kotlinc parenthesizes a receiver whose rendering is not a plain classifier.
    let receiver = receiver
        .map(|receiver| match receiver.kind() {
            TyKind::TyParam(..)
            | TyKind::Nullable(_)
            | TyKind::PlatformNullable(_)
            | TyKind::DefinitelyNotNull(_)
            | TyKind::Intersection(_)
            | TyKind::Fun(_) => format!("({}).", render(receiver)),
            _ => format!("{}.", render(receiver)),
        })
        .unwrap_or_default();
    format!(
        "{suspend}{context}{receiver}({}) -> {}",
        join(values),
        render(signature.ret)
    )
}

/// Render a nullable or platform-flexible parameter before the owner suffix supplied by the
/// diagnostic (`T? (of class Box<T>)`, not `T (of class Box<T>)?`).
pub(super) fn qualified_type_parameter(
    parameter: Ty,
    nullable: bool,
    render: &dyn Fn(&str) -> String,
) -> String {
    let TyKind::TyParam(parameter, _) = parameter.kind() else {
        unreachable!("caller only delegates type parameters")
    };
    let marker = if nullable { "?" } else { "!" };
    let rendered = render(parameter.as_str());
    let source = type_parameter_source_name(parameter.as_str());
    match rendered.strip_prefix(source) {
        Some(owner) => format!("{source}{marker}{owner}"),
        None => format!("{rendered}{marker}"),
    }
}

/// Render a platform type, including a provider-declared mutable/read-only classifier pair
/// (`MutableList<E>..List<E>` -> `(Mutable)List<E!>!`). The provider supplies the upper identity;
/// this diagnostic boundary only formats that already-normalized semantic relation.
pub(super) fn platform_type(
    inner: Ty,
    context: &[Ty],
    type_parameter: &dyn Fn(&str) -> String,
    classifier: &dyn Fn(TypeName) -> Option<String>,
    platform_upper_bound: &dyn Fn(Ty) -> Ty,
) -> String {
    let render = |ty: Ty| {
        ty.source_name_with_classifier_in(context, type_parameter, classifier, platform_upper_bound)
    };
    let upper = platform_upper_bound(inner);
    if let (TyKind::Obj(lower_owner, arguments), true) = (inner.kind(), inner.is_raw_application())
    {
        // A raw occurrence is already flexible; its interval spells both bounds, and a mapped
        // collection's read-only face is the upper one.
        let upper_owner = upper.obj_internal().unwrap_or(lower_owner);
        return raw_type(
            &render(Ty::obj_name(lower_owner)),
            &render(Ty::obj_name(upper_owner)),
            &arguments,
            render,
        );
    }
    let (TyKind::Obj(lower_owner, arguments), TyKind::Obj(upper_owner, _)) =
        (inner.kind(), upper.kind())
    else {
        return format!("{}!", render(inner));
    };
    if lower_owner == upper_owner {
        return format!("{}!", render(inner));
    }

    let face = upper_owner
        .segment_ref()
        .replace('$', ".")
        .split('.')
        .map(|segment| format!("(Mutable){segment}"))
        .collect::<Vec<_>>()
        .join(".");
    if arguments.is_empty() {
        return format!("{face}!");
    }
    let arguments = arguments
        .iter()
        .map(|argument| {
            let flexible = {
                let argument = *argument;
                match argument.kind() {
                    TyKind::TyParam(..) | TyKind::Obj(..) | TyKind::Fun(_) => {
                        Ty::platform_nullable(argument)
                    }
                    _ => argument,
                }
            };
            render(flexible)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{face}<{arguments}>!")
}

/// Render a Java raw occurrence as kotlinc spells its flexible interval: the lower bound applies
/// each formal's erasure, and the nullable upper bound stars every argument
/// (`(Box<CharSequence!>..Box<*>?)`).
pub(super) fn raw_type(
    lower: &str,
    upper: &str,
    arguments: &[Ty],
    render: impl Fn(Ty) -> String,
) -> String {
    let erased = arguments
        .iter()
        .map(|argument| render(*argument))
        .collect::<Vec<_>>()
        .join(", ");
    let stars = vec!["*"; arguments.len()].join(", ");
    format!("({lower}<{erased}>..{upper}<{stars}>?)")
}
