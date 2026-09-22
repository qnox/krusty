//! The `expect` members a classifier owes, as the common source wrote them.
//!
//! Every other rendering in this check is built from a RESOLVED signature. These cannot be: an
//! `expect` subtree is excluded from the resolved model, so neither the members nor their
//! declaring classifier is published and there is no `Ty` to read. The listing is source text
//! either way — the reference compiler shows `List<*>` and `(Int) -> String` as written, not as
//! identities — so it is rendered from the declaration syntax while that syntax is still live.

use crate::ast::{FunDecl, Param, PropDecl, TypeRef};

use super::{callable_modifiers, property_modifiers};

/// One written type, spelled as the source spelled it.
pub(super) fn written_type(ty: &TypeRef) -> String {
    if ty.is_star_projection() {
        return "*".to_string();
    }
    let mut rendered = if ty.fun_has_receiver() || !ty.fun_params.is_empty() {
        // Function syntax keeps its parameters beside the declaration and its result in `arg`. A
        // receiver rides the head of the parameter list, which is where the source wrote it.
        let (receiver, parameters) = if ty.fun_has_receiver() {
            ty.fun_params
                .split_first()
                .map_or((None, &[][..]), |(receiver, rest)| (Some(receiver), rest))
        } else {
            (None, &ty.fun_params[..])
        };
        format!(
            "{}{}({}) -> {}",
            if ty.fun_suspend() { "suspend " } else { "" },
            receiver.map_or(String::new(), |receiver| format!(
                "{}.",
                written_type(receiver)
            )),
            parameters
                .iter()
                .map(written_type)
                .collect::<Vec<_>>()
                .join(", "),
            ty.arg
                .as_ref()
                .map_or_else(|| "Unit".to_string(), |result| written_type(result)),
        )
    } else {
        format!(
            "{}{}",
            ty.name,
            if ty.targs.is_empty() {
                String::new()
            } else {
                format!(
                    "<{}>",
                    ty.targs
                        .iter()
                        .map(written_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        )
    };
    if ty.in_projection() {
        rendered = format!("in {rendered}");
    }
    if ty.out_projection() {
        rendered = format!("out {rendered}");
    }
    if ty.nullable() {
        rendered.push('?');
    }
    rendered
}

/// The declaration's own type parameters, with the bounds it wrote for them.
fn written_formals(names: &[String], bounds: &[(String, TypeRef)]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let formals = names
        .iter()
        .map(
            |name| match bounds.iter().find(|(bounded, _)| bounded == name) {
                Some((_, bound)) => format!("{name} : {}", written_type(bound)),
                None => name.clone(),
            },
        )
        .collect::<Vec<_>>();
    format!("<{}> ", formals.join(", "))
}

fn written_receiver(receiver: Option<&TypeRef>) -> String {
    receiver.map_or(String::new(), |receiver| {
        format!("{}.", written_type(receiver))
    })
}

/// A value parameter: its name and written type, plus the two facts the source owns about how it
/// is passed. A default's EXPRESSION is not shown — the reference compiler writes `= ...`.
fn written_parameter(parameter: &Param) -> String {
    format!(
        "{}{}: {}{}",
        if parameter.is_vararg { "vararg " } else { "" },
        parameter.name,
        written_type(&parameter.ty),
        if parameter.default.is_some() {
            " = ..."
        } else {
            ""
        }
    )
}

pub(super) fn written_function(function: &FunDecl) -> String {
    format!(
        "expect {}fun {}{}{}({}): {}",
        callable_modifiers(function),
        written_formals(&function.type_params, &function.type_param_bounds),
        written_receiver(function.receiver.as_ref()),
        function.name,
        function
            .params
            .iter()
            .skip(function.context_count)
            .map(written_parameter)
            .collect::<Vec<_>>()
            .join(", "),
        function
            .ret
            .as_ref()
            .map_or_else(|| "Unit".to_string(), written_type),
    )
}

/// An `expect` property always writes its type — there is no initializer to infer one from — so a
/// property without one is not rendered rather than guessed at.
pub(super) fn written_property(property: &PropDecl) -> Option<String> {
    Some(format!(
        "expect {}{} {}{}{}: {}",
        property_modifiers(property),
        if property.is_var { "var" } else { "val" },
        written_formals(&property.type_params, &property.type_param_bounds),
        written_receiver(property.receiver.as_ref()),
        property.name,
        written_type(property.ty.as_ref()?),
    ))
}
