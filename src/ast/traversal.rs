//! Does any expression under a declaration satisfy a predicate?
//!
//! One walk per declaration kind, shared by the checks that ask whether a declaration mentions
//! something at all — a `field` reference, a capture, a contract call. They are arena walks over the
//! index-based AST and carry no semantics of their own, so they live apart from the node set.

use super::*;

fn any_fun_body_expr(body: &FunBody, predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    match body {
        FunBody::Expr(expression) | FunBody::Block(expression) => predicate(*expression),
        FunBody::None => false,
    }
}

fn any_param_expr(params: &[Param], predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    for parameter in params {
        if parameter.default.is_some_and(&mut *predicate)
            || parameter
                .annotation_args
                .iter()
                .flatten()
                .copied()
                .any(&mut *predicate)
        {
            return true;
        }
    }
    false
}

pub(super) fn any_fun_decl_expr(
    function: &FunDecl,
    predicate: &mut impl FnMut(ExprId) -> bool,
) -> bool {
    function
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .any(&mut *predicate)
        || any_param_expr(&function.params, predicate)
        || any_fun_body_expr(&function.body, predicate)
}

pub(super) fn any_property_decl_expr(
    property: &PropDecl,
    predicate: &mut impl FnMut(ExprId) -> bool,
) -> bool {
    property
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .any(&mut *predicate)
        || any_param_expr(&property.context_params, predicate)
        || property.init.is_some_and(&mut *predicate)
        || property.delegate.is_some_and(&mut *predicate)
        || property
            .getter
            .as_ref()
            .is_some_and(|body| any_fun_body_expr(body, predicate))
        || property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
            .is_some_and(|body| any_fun_body_expr(body, predicate))
}

pub(super) fn any_class_decl_expr(
    class: &ClassDecl,
    predicate: &mut impl FnMut(ExprId) -> bool,
) -> bool {
    if class
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .any(&mut *predicate)
        || class.props.iter().any(|parameter| {
            parameter.default.is_some_and(&mut *predicate)
                || parameter
                    .annotation_args
                    .iter()
                    .flatten()
                    .copied()
                    .any(&mut *predicate)
        })
        || class.base_args.iter().copied().any(&mut *predicate)
        || class
            .interface_delegations
            .iter()
            .any(|delegation| predicate(delegation.value))
        || class.init_order.iter().any(|step| match step {
            ClassInit::Block(body) => predicate(*body),
            // The corresponding `body_props` entry is visited below; following the index here
            // would report the same initializer twice.
            ClassInit::PropInit(_) => false,
        })
        || class.enum_entries.iter().any(|entry| {
            entry
                .annotation_args
                .iter()
                .flatten()
                .copied()
                .chain(entry.args.iter().copied())
                .any(&mut *predicate)
                || entry.init_order.iter().any(|step| match step {
                    ClassInit::Block(body) => predicate(*body),
                    ClassInit::PropInit(_) => false,
                })
        })
    {
        return true;
    }

    for constructor in &class.secondary_ctors {
        let delegation_args = match &constructor.delegation {
            CtorDelegation::None => &[][..],
            CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
        };
        if any_param_expr(&constructor.params, predicate)
            || delegation_args.iter().copied().any(&mut *predicate)
            || constructor.body.is_some_and(&mut *predicate)
        {
            return true;
        }
    }

    class
        .methods
        .iter()
        .chain(
            class
                .enum_entries
                .iter()
                .flat_map(|entry| entry.methods.iter()),
        )
        .any(|function| any_fun_decl_expr(function, predicate))
        || class
            .body_props
            .iter()
            .chain(
                class
                    .enum_entries
                    .iter()
                    .flat_map(|entry| entry.props.iter()),
            )
            .any(|property| any_property_decl_expr(property, predicate))
}
