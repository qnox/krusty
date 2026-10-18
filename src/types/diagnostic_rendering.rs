//! Source-like type names used only at the diagnostic boundary.
//!
//! The semantic type model remains identity based. This module owns the extra spelling rules
//! needed by variance diagnostics: complete function-type shapes, nullability markers before a
//! type parameter's owner wording, and Java collection mutability flexibility.

use super::{type_parameter_source_name, FnSig, Ty, TypeName};

pub(super) fn function_type(
    signature: &FnSig,
    context: &[Ty],
    type_parameter: &dyn Fn(&str) -> String,
    classifier: &dyn Fn(TypeName) -> Option<String>,
) -> String {
    let render = |ty: Ty| ty.source_name_with_classifier_in(context, type_parameter, classifier);
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
        .map(|receiver| match receiver {
            Ty::TyParam(..)
            | Ty::Nullable(_)
            | Ty::PlatformNullable(_)
            | Ty::DefinitelyNotNull(_)
            | Ty::Intersection(_)
            | Ty::Fun(_) => format!("({}).", render(receiver)),
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
    let Ty::TyParam(parameter, _) = parameter else {
        unreachable!("caller only delegates type parameters")
    };
    let marker = if nullable { "?" } else { "!" };
    let rendered = render(parameter);
    let source = type_parameter_source_name(parameter);
    match rendered.strip_prefix(source) {
        Some(owner) => format!("{source}{marker}{owner}"),
        None => format!("{rendered}{marker}"),
    }
}

/// Render a platform type, including the Java collection pair whose lower bound is its mutable
/// face (`MutableList<E>..List<E>` -> `(Mutable)List<E!>!`).
pub(super) fn platform_type(
    inner: Ty,
    context: &[Ty],
    type_parameter: &dyn Fn(&str) -> String,
    classifier: &dyn Fn(TypeName) -> Option<String>,
) -> String {
    let Ty::Obj(name, arguments) = inner else {
        return format!(
            "{}!",
            inner.source_name_with_classifier_in(context, type_parameter, classifier)
        );
    };
    let Some(face) = mutable_collection_face(name) else {
        return format!(
            "{}!",
            inner.source_name_with_classifier_in(context, type_parameter, classifier)
        );
    };
    if arguments.is_empty() {
        return format!("{face}!");
    }
    let arguments = arguments
        .iter()
        .map(|argument| {
            let flexible = match *argument {
                Ty::TyParam(..) | Ty::Obj(..) | Ty::Fun(_) => Ty::platform_nullable(*argument),
                argument => argument,
            };
            flexible.source_name_with_classifier_in(context, type_parameter, classifier)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{face}<{arguments}>!")
}

/// kotlinc's diagnostic face for a Java collection, keyed by the selected mutable classifier
/// identity rather than by a rendered source spelling.
fn mutable_collection_face(name: TypeName) -> Option<&'static str> {
    [
        ("kotlin/collections/MutableIterable", "(Mutable)Iterable"),
        (
            "kotlin/collections/MutableCollection",
            "(Mutable)Collection",
        ),
        ("kotlin/collections/MutableList", "(Mutable)List"),
        ("kotlin/collections/MutableSet", "(Mutable)Set"),
        ("kotlin/collections/MutableMap", "(Mutable)Map"),
        (
            "kotlin/collections/MutableMap$MutableEntry",
            "(Mutable)Map.(Mutable)Entry",
        ),
        ("kotlin/collections/MutableIterator", "(Mutable)Iterator"),
        (
            "kotlin/collections/MutableListIterator",
            "(Mutable)ListIterator",
        ),
    ]
    .into_iter()
    .find(|(mutable, _)| name.matches(mutable))
    .map(|(_, face)| face)
}
