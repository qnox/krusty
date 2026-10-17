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
