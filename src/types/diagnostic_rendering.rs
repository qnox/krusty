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
    // Bare classifiers and type parameters need no parentheses; compound types and a type
    // parameter's diagnostic owner wording do.
    let receiver = receiver
        .map(|receiver| match receiver.kind() {
            TyKind::TyParam(parameter, _) | TyKind::Var(parameter, _) => {
                let rendered = render(receiver);
                if rendered == type_parameter_source_name(parameter.as_str()) {
                    format!("{rendered}.")
                } else {
                    format!("({rendered}).")
                }
            }
            TyKind::Nullable(_)
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
    let (TyKind::TyParam(parameter, _) | TyKind::Var(parameter, _)) = parameter.kind() else {
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
/// this diagnostic boundary only formats that already-normalized semantic relation. A non-null
/// collection whose only flexibility is its mutability (`nullable` false) drops the `!`.
pub(super) fn platform_type(
    inner: Ty,
    nullable: bool,
    context: &[Ty],
    type_parameter: &dyn Fn(&str) -> String,
    classifier: &dyn Fn(TypeName) -> Option<String>,
    platform_upper_bound: &dyn Fn(Ty) -> Ty,
) -> String {
    let render = |ty: Ty| {
        ty.source_name_with_classifier_in(context, type_parameter, classifier, platform_upper_bound)
    };
    let upper = platform_upper_bound(inner);
    let marker = if nullable { "!" } else { "" };
    if let (TyKind::Obj(lower_owner, arguments), true) = (inner.kind(), inner.is_raw_application())
    {
        // A raw occurrence is already flexible; its interval spells both bounds, and a mapped
        // collection's read-only face is the upper one.
        let upper_owner = upper.obj_internal().unwrap_or(lower_owner);
        return raw_type(
            &render(Ty::obj_name(lower_owner)),
            &render(Ty::obj_name(upper_owner)),
            &arguments,
            if nullable {
                RawNullability::PLATFORM
            } else {
                RawNullability::NOT_NULL
            },
            render,
        );
    }
    let (TyKind::Obj(lower_owner, arguments), TyKind::Obj(upper_owner, _)) =
        (inner.kind(), upper.kind())
    else {
        return format!("{}{marker}", render(inner));
    };
    if lower_owner == upper_owner {
        return format!("{}{marker}", render(inner));
    }

    let face = upper_owner
        .segment_ref()
        .replace('$', ".")
        .split('.')
        .map(|segment| format!("(Mutable){segment}"))
        .collect::<Vec<_>>()
        .join(".");
    if arguments.is_empty() {
        return format!("{face}{marker}");
    }
    let arguments = arguments
        .iter()
        .map(|argument| {
            let flexible = {
                let argument = *argument;
                match argument.kind() {
                    TyKind::TyParam(..) | TyKind::Var(..) | TyKind::Obj(..) | TyKind::Fun(_) => {
                        Ty::platform_nullable(argument)
                    }
                    _ => argument,
                }
            };
            render(flexible)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{face}<{arguments}>{marker}")
}

/// Render an `@Nullable` Java raw occurrence: both faces of its interval are nullable.
pub(super) fn nullable_raw_type(
    inner: Ty,
    render: impl Fn(Ty) -> String,
    platform_upper_bound: &dyn Fn(Ty) -> Ty,
) -> String {
    let TyKind::Obj(lower_owner, arguments) = inner.kind() else {
        return format!("{}?", render(inner));
    };
    let upper_owner = platform_upper_bound(inner)
        .obj_internal()
        .unwrap_or(lower_owner);
    raw_type(
        &render(Ty::obj_name(lower_owner)),
        &render(Ty::obj_name(upper_owner)),
        &arguments,
        RawNullability::NULLABLE,
        render,
    )
}

/// Which faces of a Java raw interval are nullable. An unannotated occurrence has a nullable upper
/// face only; `@NotNull` makes neither nullable and `@Nullable` both.
#[derive(Clone, Copy)]
pub(super) struct RawNullability {
    pub(super) lower: bool,
    pub(super) upper: bool,
}

impl RawNullability {
    pub(super) const NOT_NULL: Self = Self {
        lower: false,
        upper: false,
    };
    pub(super) const PLATFORM: Self = Self {
        lower: false,
        upper: true,
    };
    pub(super) const NULLABLE: Self = Self {
        lower: true,
        upper: true,
    };
}

/// Render a Java raw occurrence as kotlinc spells its flexible interval: the lower bound applies
/// each formal's erasure, and the upper bound stars every argument
/// (`(Box<CharSequence!>..Box<*>?)`).
pub(super) fn raw_type(
    lower: &str,
    upper: &str,
    arguments: &[Ty],
    nullability: RawNullability,
    render: impl Fn(Ty) -> String,
) -> String {
    let erased = arguments
        .iter()
        .map(|argument| render(*argument))
        .collect::<Vec<_>>()
        .join(", ");
    let stars = vec!["*"; arguments.len()].join(", ");
    let mark = |nullable: bool| if nullable { "?" } else { "" };
    format!(
        "({lower}<{erased}>{}..{upper}<{stars}>{})",
        mark(nullability.lower),
        mark(nullability.upper)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_receiver_parentheses_preserve_type_parameter_owner_wording() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let function = Ty::fun_with_shape(vec![parameter], Ty::String, 0, true, false);
        assert_eq!(function.source_name(), "T.() -> String");
        assert_eq!(
            function.source_name_with_type_parameter(&|_| "T (of fun <T> callback)".to_owned()),
            "(T (of fun <T> callback)).() -> String"
        );
    }
}
