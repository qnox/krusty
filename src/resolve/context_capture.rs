use std::collections::HashMap;

use crate::ast::{Expr, ExprId, File, Stmt, StmtId};
use crate::resolve::{
    ImplicitReceiverSelection, ResolvedCall, ResolvedContextArgument, ResolvedSuperCall,
    StmtLowering,
};
use crate::types::Ty;

/// Semantic selections consumed by context-capture analysis. Keeping the related checker handoff
/// maps together prevents each new selected operation from widening the shared traversal's API.
pub(crate) struct SelectedContextSources<'a> {
    pub(crate) expr_types: &'a [Ty],
    pub(crate) implicit_receiver_selections: &'a HashMap<ExprId, ImplicitReceiverSelection>,
    pub(crate) context_args: &'a HashMap<ExprId, Vec<ResolvedContextArgument>>,
    pub(crate) resolved_calls: &'a HashMap<ExprId, ResolvedCall>,
    pub(crate) resolved_super_calls: &'a HashMap<ExprId, ResolvedSuperCall>,
    pub(crate) stmt_lowers: &'a HashMap<StmtId, StmtLowering>,
}

/// Every checker-selected context source used by `body`, normalized to the callable boundary that
/// owns `body`. Direct implicit receivers stay separate because their receiver coordinates still
/// include that callable's own receiver rungs; sources used through nested callables have those
/// nested bindings and receiver rungs removed.
pub(crate) fn selected_context_values(
    file: &File,
    sources: SelectedContextSources<'_>,
    body: ExprId,
    deep: bool,
) -> (Vec<ResolvedContextArgument>, Vec<ImplicitReceiverSelection>) {
    #[derive(Clone, Copy, Default)]
    struct NestedReceiverCounts {
        all: usize,
        implicit_this: usize,
    }

    #[derive(Default)]
    struct ScanState {
        nested_bound_names: Vec<String>,
        out: Vec<ResolvedContextArgument>,
        direct_implicit_receivers: Vec<ImplicitReceiverSelection>,
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

    fn scan(
        file: &File,
        sources: &SelectedContextSources<'_>,
        e: ExprId,
        deep: bool,
        nested_callable: bool,
        nested_receiver_count: NestedReceiverCounts,
        state: &mut ScanState,
    ) {
        if let Some(selection) = sources.implicit_receiver_selections.get(&e) {
            record_source(
                &mut state.out,
                &mut state.direct_implicit_receivers,
                &ResolvedContextArgument::ImplicitReceiver(selection.clone()),
                nested_callable,
                &state.nested_bound_names,
                nested_receiver_count,
            );
        }
        if let Some(target) = sources.resolved_super_calls.get(&e) {
            record_source(
                &mut state.out,
                &mut state.direct_implicit_receivers,
                &ResolvedContextArgument::ImplicitReceiver(target.receiver.clone()),
                nested_callable,
                &state.nested_bound_names,
                nested_receiver_count,
            );
        }
        if let Some(context_args) = sources.context_args.get(&e) {
            for source in context_args {
                record_source(
                    &mut state.out,
                    &mut state.direct_implicit_receivers,
                    source,
                    nested_callable,
                    &state.nested_bound_names,
                    nested_receiver_count,
                );
            }
        }
        if let Some(call) = sources.resolved_calls.get(&e) {
            match call {
                ResolvedCall::TopLevel(target) => {
                    for source in target.context_args.iter().flatten() {
                        record_source(
                            &mut state.out,
                            &mut state.direct_implicit_receivers,
                            source,
                            nested_callable,
                            &state.nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::Extension(target) => {
                    for source in target.context_args.iter().flatten() {
                        record_source(
                            &mut state.out,
                            &mut state.direct_implicit_receivers,
                            source,
                            nested_callable,
                            &state.nested_bound_names,
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
                        &mut state.out,
                        &mut state.direct_implicit_receivers,
                        &ResolvedContextArgument::ImplicitReceiver(dispatch_receiver.clone()),
                        nested_callable,
                        &state.nested_bound_names,
                        nested_receiver_count,
                    );
                    for source in context_args.iter().flatten() {
                        record_source(
                            &mut state.out,
                            &mut state.direct_implicit_receivers,
                            source,
                            nested_callable,
                            &state.nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::LocalFunction(target) => {
                    if let Some(StmtLowering::LocalFunction(function)) =
                        sources.stmt_lowers.get(&target.stmt_id)
                    {
                        for capture in &function.captures {
                            record_source(
                                &mut state.out,
                                &mut state.direct_implicit_receivers,
                                &ResolvedContextArgument::Binding {
                                    name: capture.name.clone(),
                                    shadow_depth: 0,
                                },
                                nested_callable,
                                &state.nested_bound_names,
                                nested_receiver_count,
                            );
                        }
                    }
                    for source in &target.context_args {
                        record_source(
                            &mut state.out,
                            &mut state.direct_implicit_receivers,
                            source,
                            nested_callable,
                            &state.nested_bound_names,
                            nested_receiver_count,
                        );
                    }
                }
                ResolvedCall::Member(_) | ResolvedCall::Companion(_) => {}
            }
        }
        if let Expr::Block { stmts, trailing } = file.expr(e) {
            let saved_bound_len = state.nested_bound_names.len();
            for &statement in stmts {
                if let Some(StmtLowering::SuperPropertyWrite { target }) =
                    sources.stmt_lowers.get(&statement)
                {
                    record_source(
                        &mut state.out,
                        &mut state.direct_implicit_receivers,
                        &ResolvedContextArgument::ImplicitReceiver(target.receiver.clone()),
                        nested_callable,
                        &state.nested_bound_names,
                        nested_receiver_count,
                    );
                }
                if let Some(StmtLowering::BackingFieldWrite {
                    dispatch_receiver: Some(receiver),
                }) = sources.stmt_lowers.get(&statement)
                {
                    record_source(
                        &mut state.out,
                        &mut state.direct_implicit_receivers,
                        &ResolvedContextArgument::ImplicitReceiver(receiver.clone()),
                        nested_callable,
                        &state.nested_bound_names,
                        nested_receiver_count,
                    );
                }
                match file.stmt(statement) {
                    Stmt::For {
                        name, range, body, ..
                    } => {
                        scan(
                            file,
                            sources,
                            range.start,
                            deep,
                            nested_callable,
                            nested_receiver_count,
                            state,
                        );
                        scan(
                            file,
                            sources,
                            range.end,
                            deep,
                            nested_callable,
                            nested_receiver_count,
                            state,
                        );
                        state.nested_bound_names.push(name.clone());
                        scan(
                            file,
                            sources,
                            *body,
                            deep,
                            nested_callable,
                            nested_receiver_count,
                            state,
                        );
                        state.nested_bound_names.pop();
                    }
                    Stmt::ForEach {
                        name,
                        iterable,
                        body,
                        ..
                    } => {
                        scan(
                            file,
                            sources,
                            *iterable,
                            deep,
                            nested_callable,
                            nested_receiver_count,
                            state,
                        );
                        state.nested_bound_names.push(name.clone());
                        scan(
                            file,
                            sources,
                            *body,
                            deep,
                            nested_callable,
                            nested_receiver_count,
                            state,
                        );
                        state.nested_bound_names.pop();
                    }
                    // Local functions are lifted and own a separate capture ABI. Their bodies must
                    // not influence the enclosing lambda's capture inventory.
                    Stmt::LocalFun(_) => {}
                    _ => {
                        file.any_child_stmt(statement, &mut |child| {
                            scan(
                                file,
                                sources,
                                child,
                                deep,
                                nested_callable,
                                nested_receiver_count,
                                state,
                            );
                            false
                        });
                    }
                }
                match file.stmt(statement) {
                    Stmt::Local { name, .. }
                    | Stmt::LocalLateinit { name, .. }
                    | Stmt::LocalDelegate { name, .. } => {
                        state.nested_bound_names.push(name.clone())
                    }
                    Stmt::Destructure { entries, .. } => state.nested_bound_names.extend(
                        entries
                            .iter()
                            .filter(|entry| !entry.ignored)
                            .map(|entry| entry.name.clone()),
                    ),
                    _ => {}
                }
            }
            if let Some(trailing) = trailing {
                scan(
                    file,
                    sources,
                    *trailing,
                    deep,
                    nested_callable,
                    nested_receiver_count,
                    state,
                );
            }
            state.nested_bound_names.truncate(saved_bound_len);
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
                sources,
                *body,
                deep,
                nested_callable,
                nested_receiver_count,
                state,
            );
            for catch in catches {
                state.nested_bound_names.push(catch.name.clone());
                scan(
                    file,
                    sources,
                    catch.body,
                    deep,
                    nested_callable,
                    nested_receiver_count,
                    state,
                );
                state.nested_bound_names.pop();
            }
            if let Some(finally) = finally {
                scan(
                    file,
                    sources,
                    *finally,
                    deep,
                    nested_callable,
                    nested_receiver_count,
                    state,
                );
            }
            return;
        }
        let lambda_sig = match sources
            .expr_types
            .get(e.0 as usize)
            .copied()
            .unwrap_or(Ty::Error)
        {
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
            let saved_bound_len = state.nested_bound_names.len();
            if let Some(params) = &nested_params {
                state.nested_bound_names.extend(params.iter().cloned());
            }
            scan(
                file,
                sources,
                child,
                deep,
                nested_callable || expression_is_lambda,
                NestedReceiverCounts {
                    all: nested_receiver_count.all + expression_receiver_count,
                    implicit_this: nested_receiver_count.implicit_this
                        + expression_implicit_this_count,
                },
                state,
            );
            state.nested_bound_names.truncate(saved_bound_len);
        }
    }

    let mut state = ScanState::default();
    scan(
        file,
        &sources,
        body,
        deep,
        false,
        NestedReceiverCounts::default(),
        &mut state,
    );
    (state.out, state.direct_implicit_receivers)
}
