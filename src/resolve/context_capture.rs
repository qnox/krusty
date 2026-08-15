use std::collections::HashMap;

use crate::ast::{Expr, ExprId, File, Stmt, StmtId};
use crate::resolve::{
    ImplicitReceiverSelection, ResolvedCall, ResolvedContextArgument, StmtLowering,
};
use crate::types::Ty;

/// Every checker-selected context source used by `body`, normalized to the callable boundary that
/// owns `body`. Direct implicit receivers stay separate because their receiver coordinates still
/// include that callable's own receiver rungs; sources used through nested callables have those
/// nested bindings and receiver rungs removed.
pub(crate) fn selected_context_values(
    file: &File,
    expr_types: &[Ty],
    implicit_receiver_selections: &HashMap<ExprId, ImplicitReceiverSelection>,
    context_args: &HashMap<ExprId, Vec<ResolvedContextArgument>>,
    resolved_calls: &HashMap<ExprId, ResolvedCall>,
    stmt_lowers: &HashMap<StmtId, StmtLowering>,
    body: ExprId,
    deep: bool,
) -> (Vec<ResolvedContextArgument>, Vec<ImplicitReceiverSelection>) {
    #[derive(Clone, Copy, Default)]
    struct NestedReceiverCounts {
        all: usize,
        implicit_this: usize,
    }

    fn push_unique(out: &mut Vec<ResolvedContextArgument>, source: &ResolvedContextArgument) {
        if !out.contains(source) {
            out.push(source.clone());
        }
    }

    fn record_source(
        out: &mut Vec<ResolvedContextArgument>,
        direct_implicit_receivers: &mut Vec<ImplicitReceiverSelection>,
        source: &ResolvedContextArgument,
        nested_callable: bool,
        nested_bound_names: &[String],
        nested_receiver_count: NestedReceiverCounts,
    ) {
        if !nested_callable {
            if let ResolvedContextArgument::ImplicitReceiver(selection) = source {
                let mut selection = selection.clone();
                if let Some((name, shadow_depth)) = selection.context_binding.as_mut() {
                    let body_local_depth = nested_bound_names
                        .iter()
                        .filter(|bound| *bound == name)
                        .count()
                        + usize::from(name == "this") * nested_receiver_count.implicit_this;
                    let Some(normalized_depth) = shadow_depth.checked_sub(body_local_depth) else {
                        return;
                    };
                    *shadow_depth = normalized_depth;
                }
                if !direct_implicit_receivers.contains(&selection) {
                    direct_implicit_receivers.push(selection);
                }
                return;
            }
        }
        let normalized = match source {
            ResolvedContextArgument::Binding { name, shadow_depth } => {
                let nested_depth = nested_bound_names
                    .iter()
                    .filter(|bound| *bound == name)
                    .count();
                let Some(shadow_depth) = shadow_depth.checked_sub(nested_depth) else {
                    return;
                };
                ResolvedContextArgument::Binding {
                    name: name.clone(),
                    shadow_depth,
                }
            }
            ResolvedContextArgument::ImplicitReceiver(selection) => {
                let mut selection = selection.clone();
                if let Some((name, shadow_depth)) = selection.context_binding.as_mut() {
                    let nested_depth = nested_bound_names
                        .iter()
                        .filter(|bound| *bound == name)
                        .count()
                        + usize::from(name == "this") * nested_receiver_count.implicit_this;
                    let Some(normalized_depth) = shadow_depth.checked_sub(nested_depth) else {
                        return;
                    };
                    *shadow_depth = normalized_depth;
                } else if selection.singleton.is_none() {
                    let Some(receiver_depth) = selection
                        .receiver_depth
                        .checked_sub(nested_receiver_count.all)
                    else {
                        return;
                    };
                    selection.receiver_depth = receiver_depth;
                    selection.current = receiver_depth == 0;
                }
                ResolvedContextArgument::ImplicitReceiver(selection)
            }
        };
        push_unique(out, &normalized);
    }

    #[allow(clippy::too_many_arguments)]
    fn scan(
        file: &File,
        expr_types: &[Ty],
        implicit_receiver_selections: &HashMap<ExprId, ImplicitReceiverSelection>,
        context_args: &HashMap<ExprId, Vec<ResolvedContextArgument>>,
        resolved_calls: &HashMap<ExprId, ResolvedCall>,
        stmt_lowers: &HashMap<StmtId, StmtLowering>,
        e: ExprId,
        deep: bool,
        nested_callable: bool,
        nested_bound_names: &mut Vec<String>,
        nested_receiver_count: NestedReceiverCounts,
        out: &mut Vec<ResolvedContextArgument>,
        direct_implicit_receivers: &mut Vec<ImplicitReceiverSelection>,
    ) {
        if let Some(selection) = implicit_receiver_selections.get(&e) {
            record_source(
                out,
                direct_implicit_receivers,
                &ResolvedContextArgument::ImplicitReceiver(selection.clone()),
                nested_callable,
                nested_bound_names,
                nested_receiver_count,
            );
        }
        if let Some(sources) = context_args.get(&e) {
            for source in sources {
                record_source(
                    out,
                    direct_implicit_receivers,
                    source,
                    nested_callable,
                    nested_bound_names,
                    nested_receiver_count,
                );
            }
        }
        if let Some(call) = resolved_calls.get(&e) {
            match call {
                ResolvedCall::TopLevel(target) => {
                    for source in target.context_args.iter().flatten() {
                        record_source(
                            out,
                            direct_implicit_receivers,
                            source,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::Extension(target) => {
                    for source in &target.context_args {
                        record_source(
                            out,
                            direct_implicit_receivers,
                            source,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::MemberExtension {
                    dispatch_receiver,
                    context_args,
                    ..
                } => {
                    record_source(
                        out,
                        direct_implicit_receivers,
                        &ResolvedContextArgument::ImplicitReceiver(dispatch_receiver.clone()),
                        nested_callable,
                        nested_bound_names,
                        nested_receiver_count,
                    );
                    for source in context_args.iter().flatten() {
                        record_source(
                            out,
                            direct_implicit_receivers,
                            source,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::LocalFunction(target) => {
                    if let Some(StmtLowering::LocalFunction(function)) =
                        stmt_lowers.get(&target.stmt_id)
                    {
                        for capture in &function.captures {
                            record_source(
                                out,
                                direct_implicit_receivers,
                                &ResolvedContextArgument::Binding {
                                    name: capture.name.clone(),
                                    shadow_depth: 0,
                                },
                                nested_callable,
                                nested_bound_names,
                                nested_receiver_count,
                            );
                        }
                    }
                    for source in &target.context_args {
                        record_source(
                            out,
                            direct_implicit_receivers,
                            source,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::Member(_) | ResolvedCall::Companion(_) => {}
            }
        }
        if let Expr::Block { stmts, trailing } = file.expr(e) {
            let saved_bound_len = nested_bound_names.len();
            for &statement in stmts {
                match file.stmt(statement) {
                    Stmt::For {
                        name, range, body, ..
                    } => {
                        scan(
                            file,
                            expr_types,
                            implicit_receiver_selections,
                            context_args,
                            resolved_calls,
                            stmt_lowers,
                            range.start,
                            deep,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                            out,
                            direct_implicit_receivers,
                        );
                        scan(
                            file,
                            expr_types,
                            implicit_receiver_selections,
                            context_args,
                            resolved_calls,
                            stmt_lowers,
                            range.end,
                            deep,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                            out,
                            direct_implicit_receivers,
                        );
                        nested_bound_names.push(name.clone());
                        scan(
                            file,
                            expr_types,
                            implicit_receiver_selections,
                            context_args,
                            resolved_calls,
                            stmt_lowers,
                            *body,
                            deep,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                            out,
                            direct_implicit_receivers,
                        );
                        nested_bound_names.pop();
                    }
                    Stmt::ForEach {
                        name,
                        iterable,
                        body,
                        ..
                    } => {
                        scan(
                            file,
                            expr_types,
                            implicit_receiver_selections,
                            context_args,
                            resolved_calls,
                            stmt_lowers,
                            *iterable,
                            deep,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                            out,
                            direct_implicit_receivers,
                        );
                        nested_bound_names.push(name.clone());
                        scan(
                            file,
                            expr_types,
                            implicit_receiver_selections,
                            context_args,
                            resolved_calls,
                            stmt_lowers,
                            *body,
                            deep,
                            nested_callable,
                            nested_bound_names,
                            nested_receiver_count,
                            out,
                            direct_implicit_receivers,
                        );
                        nested_bound_names.pop();
                    }
                    // Local functions are lifted and own a separate capture ABI. Their bodies must
                    // not influence the enclosing lambda's capture inventory.
                    Stmt::LocalFun(_) => {}
                    _ => {
                        file.any_child_stmt(statement, &mut |child| {
                            scan(
                                file,
                                expr_types,
                                implicit_receiver_selections,
                                context_args,
                                resolved_calls,
                                stmt_lowers,
                                child,
                                deep,
                                nested_callable,
                                nested_bound_names,
                                nested_receiver_count,
                                out,
                                direct_implicit_receivers,
                            );
                            false
                        });
                    }
                }
                match file.stmt(statement) {
                    Stmt::Local { name, .. }
                    | Stmt::LocalLateinit { name, .. }
                    | Stmt::LocalDelegate { name, .. } => nested_bound_names.push(name.clone()),
                    Stmt::Destructure { entries, .. } => nested_bound_names.extend(
                        entries
                            .iter()
                            .filter(|(name, _)| name != "_")
                            .map(|(name, _)| name.clone()),
                    ),
                    _ => {}
                }
            }
            if let Some(trailing) = trailing {
                scan(
                    file,
                    expr_types,
                    implicit_receiver_selections,
                    context_args,
                    resolved_calls,
                    stmt_lowers,
                    *trailing,
                    deep,
                    nested_callable,
                    nested_bound_names,
                    nested_receiver_count,
                    out,
                    direct_implicit_receivers,
                );
            }
            nested_bound_names.truncate(saved_bound_len);
            return;
        }
        if let Expr::Try {
            body,
            catches,
            finally,
        } = file.expr(e)
        {
            scan(
                file,
                expr_types,
                implicit_receiver_selections,
                context_args,
                resolved_calls,
                stmt_lowers,
                *body,
                deep,
                nested_callable,
                nested_bound_names,
                nested_receiver_count,
                out,
                direct_implicit_receivers,
            );
            for catch in catches {
                nested_bound_names.push(catch.name.clone());
                scan(
                    file,
                    expr_types,
                    implicit_receiver_selections,
                    context_args,
                    resolved_calls,
                    stmt_lowers,
                    catch.body,
                    deep,
                    nested_callable,
                    nested_bound_names,
                    nested_receiver_count,
                    out,
                    direct_implicit_receivers,
                );
                nested_bound_names.pop();
            }
            if let Some(finally) = finally {
                scan(
                    file,
                    expr_types,
                    implicit_receiver_selections,
                    context_args,
                    resolved_calls,
                    stmt_lowers,
                    *finally,
                    deep,
                    nested_callable,
                    nested_bound_names,
                    nested_receiver_count,
                    out,
                    direct_implicit_receivers,
                );
            }
            return;
        }
        let lambda_sig = match expr_types.get(e.0 as usize).copied().unwrap_or(Ty::Error) {
            Ty::Fun(sig) => Some(sig),
            _ => None,
        };
        let nested_params = match file.expr(e) {
            Expr::Lambda { params, .. } if !params.is_empty() => Some(params.clone()),
            Expr::Lambda { .. } if !file.anon_fun_lambdas.contains(&e.0) => {
                let implicit_it = match lambda_sig {
                    Some(sig) => {
                        let implicit_count = sig.context_count + usize::from(sig.has_receiver);
                        sig.params.len().saturating_sub(implicit_count) == 1
                    }
                    None => false,
                };
                Some(if implicit_it {
                    vec!["it".to_string()]
                } else {
                    Vec::new()
                })
            }
            Expr::Lambda { .. } => Some(Vec::new()),
            _ => None,
        };
        let expression_receiver_count = if nested_params.is_some() {
            lambda_sig.map_or(0, |sig| sig.context_count + usize::from(sig.has_receiver))
        } else {
            0
        };
        let expression_named_context_count = if nested_params.is_some() {
            lambda_sig.map_or(0, |sig| {
                (file.anon_fun_context_count.get(&e.0).copied().unwrap_or(0) as usize)
                    .min(sig.context_count)
            })
        } else {
            0
        };
        let expression_implicit_this_count =
            expression_receiver_count.saturating_sub(expression_named_context_count);
        let expression_is_lambda = nested_params.is_some();
        if !deep && expression_is_lambda {
            return;
        }
        let children = std::cell::RefCell::new(Vec::new());
        file.any_child_expr(
            e,
            &mut |child| {
                children.borrow_mut().push(child);
                false
            },
            &mut |statement| {
                file.any_child_stmt(statement, &mut |child| {
                    children.borrow_mut().push(child);
                    false
                });
                false
            },
        );
        for child in children.into_inner() {
            let saved_bound_len = nested_bound_names.len();
            if let Some(params) = &nested_params {
                nested_bound_names.extend(params.iter().cloned());
            }
            scan(
                file,
                expr_types,
                implicit_receiver_selections,
                context_args,
                resolved_calls,
                stmt_lowers,
                child,
                deep,
                nested_callable || expression_is_lambda,
                nested_bound_names,
                NestedReceiverCounts {
                    all: nested_receiver_count.all + expression_receiver_count,
                    implicit_this: nested_receiver_count.implicit_this
                        + expression_implicit_this_count,
                },
                out,
                direct_implicit_receivers,
            );
            nested_bound_names.truncate(saved_bound_len);
        }
    }

    let mut out = Vec::new();
    let mut direct_implicit_receivers = Vec::new();
    let mut nested_bound_names = Vec::new();
    scan(
        file,
        expr_types,
        implicit_receiver_selections,
        context_args,
        resolved_calls,
        stmt_lowers,
        body,
        deep,
        false,
        &mut nested_bound_names,
        NestedReceiverCounts::default(),
        &mut out,
        &mut direct_implicit_receivers,
    );
    (out, direct_implicit_receivers)
}
