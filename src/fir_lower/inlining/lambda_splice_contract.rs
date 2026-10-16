//! Mutation-free validation shared by declared inline-lambda splices.
//!
//! The contract owns the parameter-shape and local-slot calculations that must succeed before an
//! expansion consumes any implementation body.

use crate::fx_hash::HashSet;
use crate::ir::{ExprId, IrExpr};

/// How a spliced lambda's parameters meet the arguments of its invocation.
#[derive(Clone, Copy)]
pub(super) enum LambdaParameterBinding<'callee> {
    /// Named parameters become locals holding their arguments; `callee` opens the inline frame.
    Declared { callee: Option<&'callee str> },
    /// Parameters directly reuse value slots supplied by their arguments.
    Remapped,
}

/// Whether `invocation` and its inline `lambda` have the parameter shape the splicer consumes.
pub(super) fn invocation_is_spliceable(
    ir: &crate::ir::IrFile,
    invocation: ExprId,
    lambda: ExprId,
) -> bool {
    let IrExpr::InvokeFunction { args, .. } = ir.expr(invocation) else {
        return false;
    };
    let arg_count = args.len();
    match ir.expr(lambda).clone() {
        IrExpr::Lambda {
            impl_fn,
            arity,
            captures,
            inline_body: Some(_),
            ..
        } => {
            let suspend = ir.suspend_funs.contains(&impl_fn);
            arg_count + usize::from(suspend) == usize::from(arity)
                && ir
                    .functions
                    .get(impl_fn as usize)
                    .is_some_and(|function| function.params.len() == captures.len() + arg_count)
        }
        IrExpr::CallableReference(_) => {
            let Some((impl_fn, captures, _, arity)) =
                super::super::source_calls::inline_argument_template(ir, lambda)
            else {
                return false;
            };
            arg_count == arity
                && ir
                    .functions
                    .get(impl_fn as usize)
                    .is_some_and(|function| function.params.len() == captures.len() + arg_count)
        }
        _ => false,
    }
}

/// Validate every fallible shape and local-index calculation without allocating expressions or
/// consuming an implementation body.
pub(super) fn preflight(
    ir: &crate::ir::IrFile,
    invocation: ExprId,
    mut next_temporary: u32,
) -> Option<u32> {
    let IrExpr::InvokeFunction {
        func, args, params, ..
    } = ir.expr(invocation)
    else {
        return None;
    };
    if !invocation_is_spliceable(ir, invocation, *func) {
        return None;
    }
    let (implementation, captures, inline_body, indexed_parameters) = match ir.expr(*func) {
        IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body: Some(inline_body),
            ..
        } => (*impl_fn, captures.clone(), *inline_body, false),
        IrExpr::CallableReference(_) => {
            let (implementation, captures, inline_body, _) =
                super::super::source_calls::inline_argument_template(ir, *func)?;
            (implementation, captures, inline_body, true)
        }
        _ => return None,
    };
    let parameter_types = &ir.functions.get(implementation as usize)?.params;
    if captures.len().checked_add(args.len())? != parameter_types.len()
        || params.len() != args.len()
        || u32::try_from(parameter_types.len()).is_err()
    {
        return None;
    }
    let receiver = ir
        .lambda_origins
        .get(&implementation)
        .and_then(|origin| origin.receiver_parameter);
    let identities = ir.function_parameter_identities(implementation);
    for (parameter, value) in captures.iter().chain(args).copied().enumerate() {
        let parameter = u32::try_from(parameter).ok()?;
        let reference_parameter = indexed_parameters
            .then(|| (parameter as usize).checked_sub(captures.len()))
            .flatten();
        let source_name = if reference_parameter.is_some() {
            None
        } else {
            (parameter as usize >= captures.len())
                .then(|| {
                    identities
                        .and_then(|identities| identities.get(parameter as usize))
                        .and_then(|identity| identity.source_name.as_ref())
                })
                .flatten()
        };
        let reuses_slot = receiver != Some(parameter)
            && reference_parameter.is_none()
            && source_name.is_none()
            && matches!(ir.expr(value), IrExpr::GetValue(_));
        if !reuses_slot {
            next_temporary = next_temporary.checked_add(1)?;
        }
    }
    let formal_count = u32::try_from(parameter_types.len()).ok()?;
    let local_count = local_count(ir, inline_body, formal_count, next_temporary)?;
    next_temporary.checked_add(local_count)
}

fn local_count(
    ir: &crate::ir::IrFile,
    root: ExprId,
    formal_count: u32,
    local_base: u32,
) -> Option<u32> {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut local_count = 0;
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        for index in value_indices(ir.expr(expression)) {
            if index < formal_count {
                continue;
            }
            let local = index.checked_sub(formal_count)?;
            local_base.checked_add(local)?;
            local_count = local_count.max(local.checked_add(1)?);
        }
        if let IrExpr::Lambda { captures, .. } = ir.expr(expression) {
            pending.extend(captures.iter().copied());
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    Some(local_count)
}

pub(super) fn value_indices(expression: &IrExpr) -> Vec<u32> {
    match expression {
        IrExpr::GetValue(index)
        | IrExpr::SetValue { var: index, .. }
        | IrExpr::Variable { index, .. } => vec![*index],
        IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var).collect(),
        IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop { variable, .. }) => {
            vec![*variable]
        }
        _ => Vec::new(),
    }
}
