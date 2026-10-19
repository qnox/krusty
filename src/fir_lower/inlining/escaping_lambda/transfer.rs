//! Call-site copies of lambdas that escape an inline expansion.
//!
//! A lambda stored by an inline function is not spliced into the caller. Its implementation method
//! is a separate function, so substituting types only in the inline template leaves that method on
//! the declaration's types. This module clones that implementation when the expansion fixes a
//! reified type the body uses, an anonymous class changes identity, or the closure captures an
//! exact lambda substituted for an inline parameter. A non-reified type parameter stays erased on
//! the shared implementation. The declaration's implementation stays in place for the inline
//! method's own fallback body.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{Callee, ExprId, IrCheckedOperation, IrCheckedSubstitution, IrExpr};
use crate::types::{stored_value_ty, ty_subst_keep_unbound, Ty};

/// A mutation-free decision for every escaping implementation this expansion will regenerate.
/// Applying the plan cannot decline, so no specialized function or retargeted arena node can be
/// stranded when a later phase rejects the containing inline expansion.
pub(in super::super) struct EscapingLambdaPlan {
    roots: Vec<PlannedRoot>,
    functions: HashSet<crate::ir::FunId>,
}

impl EscapingLambdaPlan {
    /// Whether no lambda escapes the expansion.
    pub(in super::super) fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.functions.is_empty()
    }
}

struct PlannedRoot {
    copy: ExprId,
    implementation: crate::ir::FunId,
    inline_captures: Vec<InlineCapture>,
}

/// Validate every escaping lambda specialization without changing common IR.
pub(in super::super) fn plan(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    protected: &HashSet<ExprId>,
    directly_spliced_lambdas: &HashSet<ExprId>,
    substituted_inline_lambdas: &HashSet<ExprId>,
    anonymous_owned_substitution_edges: &HashSet<(ExprId, ExprId)>,
    reified_bindings: &HashMap<String, Ty>,
) -> Option<EscapingLambdaPlan> {
    let mut decisions = HashMap::<u32, bool>::default();
    let mut functions = HashSet::default();
    let mut requests = HashMap::<crate::ir::FunId, SpecializationRequest>::default();
    let mut visiting = HashSet::default();
    let mut roots = Vec::new();
    let anonymous_owned_lambdas = anonymous_owned_substitution_edges
        .iter()
        .map(|(constructor_lambda, _)| *constructor_lambda)
        .collect::<HashSet<_>>();
    let mut lambdas = copies
        .iter()
        .copied()
        .filter(|(source, _)| !protected.contains(source))
        .filter(|(_, copy)| matches!(ir.expr(*copy), IrExpr::Lambda { .. }))
        .collect::<Vec<_>>();
    let mut plan_lambdas = plan_lambda_implementations(
        ir,
        copies
            .iter()
            .filter(|(source, _)| !protected.contains(source))
            .map(|(_, copy)| *copy),
    )
    .into_iter()
    .map(|(expression, _)| (expression, expression))
    .collect::<Vec<_>>();
    lambdas.append(&mut plan_lambdas);
    lambdas.sort_unstable_by_key(|&(_, expression)| expression);
    lambdas.dedup_by_key(|(_, expression)| *expression);
    for (_, copy) in lambdas {
        // A directly invoked lambda is consumed by the declared-lambda splicer below this plan;
        // it has no escaping value or class to specialize. In particular, its declaration
        // implementation may already have been consumed by an earlier expansion of the same
        // inline template while its retained inline body remains valid for this splice.
        if directly_spliced_lambdas.contains(&copy) {
            continue;
        }
        // A substituted lambda remains a caller-owned value until a declaration closure captures
        // it. That ownership edge transfers its captures and body into the call-site copy. The exact
        // declaration lambda whose anonymous-object constructor owns a substituted capture is also
        // handled there: anonymous specialization either retains the supplied closure or consumes
        // it in the copied methods.
        if caller_capturing_substitution(ir, copy, substituted_inline_lambdas)
            || anonymous_owned_lambdas.contains(&copy)
        {
            continue;
        }
        let IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body,
            ..
        } = ir.expr(copy)
        else {
            continue;
        };
        let mut inline_captures = captures
            .iter()
            .enumerate()
            .filter(|(_, capture)| {
                substituted_inline_lambdas.contains(capture)
                    && !anonymous_owned_substitution_edges.contains(&(copy, **capture))
            })
            .map(|(parameter, &lambda)| InlineCapture { parameter, lambda })
            .collect::<Vec<_>>();
        if let Some(inline_body) = *inline_body {
            if !inline_captures.is_empty() {
                let shape = ir.functions.get(*impl_fn as usize)?;
                if !inline_capture_splices_are_valid(
                    ir,
                    inline_body,
                    *impl_fn,
                    shape,
                    &inline_captures,
                ) {
                    // Capture transfer is a representation choice, not a prerequisite for the
                    // containing inline expansion. A suspend/coroutine lambda can have an invoke
                    // shape that this direct body splice does not own; retain that exact supplied
                    // closure as the declaration closure's capture instead of rejecting the
                    // otherwise-valid inline call.
                    inline_captures.clear();
                }
            }
        }
        let planned = plan_function(
            ir,
            *impl_fn,
            copy,
            None,
            reified_bindings,
            &inline_captures,
            &mut decisions,
            &mut functions,
            &mut requests,
            &mut visiting,
        );
        match planned {
            FunctionPlan::Planned => {}
            FunctionPlan::NotNeeded => continue,
            FunctionPlan::Invalid => return None,
        }
        roots.push(PlannedRoot {
            copy,
            implementation: *impl_fn,
            inline_captures,
        });
    }
    Some(EscapingLambdaPlan { roots, functions })
}

/// A caller-owned substituted lambda is not itself a declaration closure to specialize. Its exact
/// capture edge is consumed when the declaration closure that captured it is planned.
fn caller_capturing_substitution(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    substituted_inline_lambdas: &HashSet<ExprId>,
) -> bool {
    substituted_inline_lambdas.contains(&expression)
        && matches!(ir.expr(expression), IrExpr::Lambda { captures, .. } if !captures.is_empty())
}

/// Commit a previously validated plan after every fallible transformation of the containing
/// expansion has succeeded.
pub(in super::super) fn apply(
    ir: &mut crate::ir::IrFile,
    plan: EscapingLambdaPlan,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    caller_declaration: crate::fir::DeclarationId,
    caller: Option<crate::ir::IrEnclosure>,
    caller_is_default: bool,
    caller_source_name: &str,
    inline_template_function: crate::ir::FunId,
    inline_template_owner: Option<crate::types::TypeName>,
    inline_template_source: crate::fir::SourceFileId,
    inline_callee: crate::fir::CallableId,
    inline_callee_source_name: &str,
) -> Vec<crate::ir::FunId> {
    let mut replacements = HashMap::<u32, u32>::default();
    let mut decisions = HashMap::<u32, bool>::default();
    let mut suppressions = HashSet::<crate::ir::FunId>::default();
    let mut state = Specialization {
        replacements: &mut replacements,
        decisions: &mut decisions,
        planned_functions: &plan.functions,
        suppressions: &mut suppressions,
        caller_declaration,
        caller,
        caller_is_default,
        caller_source_name,
        inline_template_function,
        inline_template_owner,
        inline_template_source,
        inline_callee,
        inline_callee_source_name,
    };
    for PlannedRoot {
        copy,
        implementation,
        inline_captures,
    } in plan.roots
    {
        let specialized = specialized_lambda_function(
            ir,
            implementation,
            bindings,
            reified_bindings,
            &mut state,
            None,
            &inline_captures,
        )
        .expect("an escaping-lambda plan commits without a second fallible decision");
        ir.specialized_expansion_order
            .entry(specialized)
            .or_insert(copy);
        if let Some(caller) = state.caller {
            ir.callable_reference_enclosures.insert(copy, caller);
        }
        let retained_inline_body = match ir.expr(copy) {
            IrExpr::Lambda {
                inline_body: Some(body),
                ..
            } if !inline_captures.is_empty() => Some(*body),
            _ => None,
        };
        if let Some(retained_inline_body) = retained_inline_body {
            let mut inline_shape = ir.functions[implementation as usize].clone();
            inline_shape.body = Some(retained_inline_body);
            splice_inline_captures(
                ir,
                retained_inline_body,
                &mut inline_shape,
                &inline_captures,
                state.inline_callee_source_name,
            )
            .expect("the plan validated the retained captured-lambda splice");
        }
        // The copied lambda's `inline_body` is a separate expression dag from the implementation.
        // Nested lambdas there still name the declaration implementations until this retarget.
        retarget_lambdas(ir, copy, state.replacements);
        if !inline_captures.is_empty() {
            let transferred = inline_captures
                .iter()
                .map(|capture| match ir.expr(capture.lambda) {
                    IrExpr::Lambda { captures, .. } => captures.clone(),
                    _ => unreachable!("the plan validated an exact substituted source lambda"),
                })
                .collect::<Vec<_>>();
            let IrExpr::Lambda { captures, .. } = &mut ir.exprs[copy as usize] else {
                unreachable!("the specialized escaping value remains a lambda");
            };
            for (capture, transferred) in inline_captures.iter().zip(transferred).rev() {
                captures.splice(capture.parameter..=capture.parameter, transferred);
            }
        }
    }
    let mut suppressions = suppressions.into_iter().collect::<Vec<_>>();
    suppressions.sort_unstable();
    suppressions
}

struct Specialization<'a> {
    replacements: &'a mut HashMap<u32, u32>,
    decisions: &'a mut HashMap<u32, bool>,
    planned_functions: &'a HashSet<crate::ir::FunId>,
    /// Source lambda implementations consumed by exact inline-argument substitution. Their bodies
    /// remain available until the containing expansion has passed every fallible transformation.
    suppressions: &'a mut HashSet<crate::ir::FunId>,
    caller_declaration: crate::fir::DeclarationId,
    caller: Option<crate::ir::IrEnclosure>,
    caller_is_default: bool,
    caller_source_name: &'a str,
    inline_template_function: crate::ir::FunId,
    inline_template_owner: Option<crate::types::TypeName>,
    inline_template_source: crate::fir::SourceFileId,
    inline_callee: crate::fir::CallableId,
    inline_callee_source_name: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InlineCapture {
    /// Parameter/capture ordinal in the escaping lambda implementation.
    parameter: usize,
    /// The exact call-site lambda substituted for that captured inline parameter.
    lambda: ExprId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FunctionPlan {
    NotNeeded,
    Planned,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpecializationRequest {
    site: ExprId,
    parent: Option<crate::ir::FunId>,
    inline_captures: Vec<InlineCapture>,
}

fn plan_function(
    ir: &crate::ir::IrFile,
    function: crate::ir::FunId,
    site: ExprId,
    parent: Option<crate::ir::FunId>,
    reified_bindings: &HashMap<String, Ty>,
    inline_captures: &[InlineCapture],
    decisions: &mut HashMap<u32, bool>,
    planned: &mut HashSet<crate::ir::FunId>,
    requests: &mut HashMap<crate::ir::FunId, SpecializationRequest>,
    visiting: &mut HashSet<crate::ir::FunId>,
) -> FunctionPlan {
    let request = SpecializationRequest {
        site,
        parent,
        inline_captures: inline_captures.to_vec(),
    };
    if let Some(previous) = requests.get(&function) {
        return if previous == &request {
            FunctionPlan::Planned
        } else {
            FunctionPlan::Invalid
        };
    }
    if !visiting.insert(function) {
        return FunctionPlan::Invalid;
    }
    let uses_reified = function_uses_binding(
        ir,
        function,
        reified_bindings,
        decisions,
        &mut HashSet::default(),
        &mut HashSet::default(),
    );
    let needed = uses_reified
        || function_mentions_anonymous_class(ir, function)
        || !inline_captures.is_empty();
    let valid = if !needed {
        visiting.remove(&function);
        return FunctionPlan::NotNeeded;
    } else if let Some(body) = ir
        .functions
        .get(function as usize)
        .and_then(|shape| shape.body)
    {
        inline_captures.is_empty()
            || inline_capture_splices_are_valid(
                ir,
                body,
                function,
                &ir.functions[function as usize],
                inline_captures,
            )
    } else {
        false
    };
    if !valid {
        visiting.remove(&function);
        return FunctionPlan::Invalid;
    }
    requests.insert(function, request);
    planned.insert(function);
    let body = ir.functions[function as usize]
        .body
        .expect("a validated escaping implementation has a body");
    for (nested_site, nested) in lambda_implementations(ir, body) {
        if plan_function(
            ir,
            nested,
            nested_site,
            Some(function),
            reified_bindings,
            &[],
            decisions,
            planned,
            requests,
            visiting,
        ) == FunctionPlan::Invalid
        {
            visiting.remove(&function);
            return FunctionPlan::Invalid;
        }
    }
    visiting.remove(&function);
    FunctionPlan::Planned
}

/// Validate the only structural transformation that can reject an escaping specialization. The
/// apply pass performs the same walk on cloned nodes, whose shapes and value ordinals are exact
/// copies of these declaration nodes.
fn inline_capture_splices_are_valid(
    ir: &crate::ir::IrFile,
    body: ExprId,
    function: crate::ir::FunId,
    shape: &crate::ir::IrFunction,
    inline_captures: &[InlineCapture],
) -> bool {
    if (!shape.param_checks.is_empty() && shape.param_checks.len() != shape.params.len())
        || !outer_parameter_sidecars_are_valid(ir, function, shape)
    {
        crate::trace_compiler!(
            "lower",
            "escaping lambda {function} has inconsistent outer parameter sidecars"
        );
        return false;
    }
    let mut captures = inline_captures.to_vec();
    captures.sort_unstable_by_key(|capture| capture.parameter);
    captures.dedup_by_key(|capture| capture.parameter);
    if captures.len() != inline_captures.len()
        || captures
            .iter()
            .any(|capture| capture.parameter >= shape.params.len())
        || replaced_parameter_checks(ir, &shape.param_checks, &captures, shape.params.len())
            .is_none()
    {
        crate::trace_compiler!(
            "lower",
            "escaping lambda {function} has invalid captured-parameter replacements {captures:?}"
        );
        return false;
    }
    let mut new_arity = shape.params.len();
    for capture in &captures {
        let IrExpr::Lambda {
            impl_fn,
            captures: transferred,
            ..
        } = ir.expr(capture.lambda)
        else {
            crate::trace_compiler!(
                "lower",
                "escaping lambda {function} capture {} is not a lambda",
                capture.lambda
            );
            return false;
        };
        if ir
            .functions
            .get(*impl_fn as usize)
            .map_or(true, |function| function.params.len() < transferred.len())
        {
            crate::trace_compiler!(
                "lower",
                "escaping lambda {function} capture implementation {impl_fn} has fewer parameters than transferred captures"
            );
            return false;
        }
        if !transferred.is_empty() {
            let Some(identities) = ir.function_parameter_identities(*impl_fn) else {
                crate::trace_compiler!(
                    "lower",
                    "escaping lambda {function} capture implementation {impl_fn} has no parameter identities"
                );
                return false;
            };
            let captured_receivers = ir
                .fn_params
                .get(impl_fn)
                .map(|parameters| parameters.captured_receivers.as_slice())
                .unwrap_or_default();
            if identities.len() != ir.functions[*impl_fn as usize].params.len()
                || ir
                    .fn_params
                    .get(impl_fn)
                    .is_some_and(|parameters| !captured_receiver_facts_are_valid(parameters))
                || ir.lambda_own_params_from.get(impl_fn).copied()
                    != u32::try_from(transferred.len()).ok()
                || !parameter_sidecars_cover(ir, *impl_fn, transferred.len())
                || identities[..transferred.len()]
                    .iter()
                    .any(|identity| match identity.role {
                        crate::ir::IrParameterRole::CapturedValue { .. } => false,
                        crate::ir::IrParameterRole::CapturedReceiver { ordinal } => {
                            captured_receivers.get(ordinal as usize).is_none()
                        }
                        _ => true,
                    })
            {
                crate::trace_compiler!(
                    "lower",
                    "escaping lambda {function} capture implementation {impl_fn} has inconsistent capture-prefix parameter facts: identities={:?} params={} own_from={:?} transferred={} sidecars_cover={} roles={:?}",
                    identities.len(),
                    ir.functions[*impl_fn as usize].params.len(),
                    ir.lambda_own_params_from.get(impl_fn),
                    transferred.len(),
                    parameter_sidecars_cover(ir, *impl_fn, transferred.len()),
                    identities
                        .iter()
                        .map(|identity| identity.role)
                        .collect::<Vec<_>>()
                );
                return false;
            }
        }
        let Some(arity) = new_arity
            .checked_sub(1)
            .and_then(|arity| arity.checked_add(transferred.len()))
        else {
            return false;
        };
        new_arity = arity;
    }
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let Some(mut local_count) = u32::try_from(shape.params.len()).ok().map(|_| 0_u32) else {
        return false;
    };
    for &expression in &expressions {
        for index in super::super::lambda_splice_contract::value_indices(ir.expr(expression)) {
            if index >= shape.params.len() as u32 {
                let Some(local) = index
                    .checked_sub(shape.params.len() as u32)
                    .and_then(|local| local.checked_add(1))
                else {
                    return false;
                };
                local_count = local_count.max(local);
            }
        }
    }
    let Some(mut next_temporary) = u32::try_from(new_arity)
        .ok()
        .and_then(|arity| arity.checked_add(local_count))
    else {
        return false;
    };
    for capture in &captures {
        let Some(parameter) = u32::try_from(capture.parameter).ok() else {
            return false;
        };
        let reads = expressions
            .iter()
            .copied()
            .filter(|&expression| matches!(ir.expr(expression), IrExpr::GetValue(index) if *index == parameter))
            .collect::<HashSet<_>>();
        let invocations = expressions
            .iter()
            .copied()
            .filter(|&expression| {
                matches!(
                    ir.expr(expression),
                    IrExpr::InvokeFunction { func, .. } if reads.contains(func)
                )
            })
            .collect::<Vec<_>>();
        let invoked_reads = invocations
            .iter()
            .filter_map(|&invocation| match ir.expr(invocation) {
                IrExpr::InvokeFunction { func, .. } => Some(*func),
                _ => None,
            })
            .collect::<HashSet<_>>();
        if reads.is_empty() || reads != invoked_reads {
            crate::trace_compiler!(
                "lower",
                "escaping lambda {function} captured parameter {parameter} is not used only as a direct invocation"
            );
            return false;
        }
        for invocation in invocations {
            if !captured_invocation_is_valid(ir, invocation, capture.lambda, &mut next_temporary) {
                crate::trace_compiler!(
                    "lower",
                    "escaping lambda {function} captured invocation {invocation} cannot be spliced"
                );
                return false;
            }
        }
    }
    true
}

fn outer_parameter_sidecars_are_valid(
    ir: &crate::ir::IrFile,
    function: crate::ir::FunId,
    shape: &crate::ir::IrFunction,
) -> bool {
    let arity = shape.params.len();
    let optional_len = |len: usize| len == 0 || len == arity;
    if let Some(info) = ir.fn_params.get(&function) {
        if info.identities.len() != arity
            || !captured_receiver_facts_are_valid(info)
            || info
                .defaults
                .as_ref()
                .is_some_and(|values| !optional_len(values.len()))
            || !optional_len(info.inline_modifiers.len())
            || !optional_len(info.expansion_modes.len())
        {
            return false;
        }
    }
    if [
        ir.fn_param_declared_nullable.get(&function).map(Vec::len),
        ir.fn_param_annotations.get(&function).map(Vec::len),
        ir.fn_param_no_infer.get(&function).map(Vec::len),
    ]
    .into_iter()
    .flatten()
    .any(|len| !optional_len(len))
    {
        return false;
    }
    if ir
        .lambda_own_params_from
        .get(&function)
        .is_some_and(|own_from| *own_from as usize > arity)
        || ir
            .fn_varargs
            .get(&function)
            .is_some_and(|vararg| vararg.index >= arity)
        || ir
            .shared_capture_parameters
            .keys()
            .any(|&(owner, ordinal)| owner == function && ordinal as usize >= arity)
    {
        return false;
    }
    true
}

fn captured_receiver_facts_are_valid(parameters: &crate::ir::FnParamInfo) -> bool {
    let mut referenced = vec![false; parameters.captured_receivers.len()];
    for identity in &parameters.identities {
        let crate::ir::IrParameterRole::CapturedReceiver { ordinal } = identity.role else {
            continue;
        };
        let Some(referenced) = referenced.get_mut(ordinal as usize) else {
            return false;
        };
        if std::mem::replace(referenced, true) {
            return false;
        }
    }
    referenced.into_iter().all(|referenced| referenced)
}

fn parameter_sidecars_cover(
    ir: &crate::ir::IrFile,
    function: crate::ir::FunId,
    count: usize,
) -> bool {
    [
        ir.fn_param_declared_nullable.get(&function).map(Vec::len),
        ir.fn_param_annotations.get(&function).map(Vec::len),
        ir.fn_param_no_infer.get(&function).map(Vec::len),
    ]
    .into_iter()
    .flatten()
    .all(|len| len >= count)
}

fn captured_invocation_is_valid(
    ir: &crate::ir::IrFile,
    invocation: ExprId,
    lambda: ExprId,
    next_temporary: &mut u32,
) -> bool {
    let IrExpr::InvokeFunction { args, params, .. } = ir.expr(invocation) else {
        return false;
    };
    let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body: Some(inline_body),
        arity,
        ..
    } = ir.expr(lambda)
    else {
        return false;
    };
    if usize::from(*arity) != args.len() || params.len() != args.len() {
        return false;
    }
    let Some(parameter_types) = ir
        .functions
        .get(*impl_fn as usize)
        .map(|function| &function.params)
    else {
        return false;
    };
    if parameter_types.len() != captures.len() + args.len() {
        return false;
    }
    let identities = ir.function_parameter_identities(*impl_fn);
    for (parameter, &value) in args.iter().enumerate() {
        let parameter = captures.len() + parameter;
        let unnamed_local = identities
            .and_then(|identities| identities.get(parameter))
            .and_then(|identity| identity.source_name.as_ref())
            .is_none()
            && matches!(ir.expr(value), IrExpr::GetValue(_));
        if !unnamed_local {
            let Some(next) = next_temporary.checked_add(1) else {
                return false;
            };
            *next_temporary = next;
        }
    }
    let formal_count = match u32::try_from(parameter_types.len()) {
        Ok(count) => count,
        Err(_) => return false,
    };
    let Some(local_count) =
        inline_body_local_count(ir, *inline_body, formal_count, *next_temporary)
    else {
        return false;
    };
    let Some(next) = next_temporary.checked_add(local_count) else {
        return false;
    };
    *next_temporary = next;
    true
}

fn inline_body_local_count(
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
        for index in super::super::lambda_splice_contract::value_indices(ir.expr(expression)) {
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

fn retarget_lambdas(ir: &mut crate::ir::IrFile, root: ExprId, replacements: &HashMap<u32, u32>) {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut plans = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let current = match ir.expr(expression) {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        };
        if let Some(current) = current {
            if let Some(done) = replacements.get(&current).copied() {
                if let IrExpr::Lambda { impl_fn, .. } = &mut ir.exprs[expression as usize] {
                    *impl_fn = done;
                }
                retarget_specialized_parent(ir, expression, done);
            }
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

/// A nested lambda copied with its implementation still names the source parent. The specialized
/// copy's parent is the specialized outer lambda, so the enclosure has to name that parent too.
fn retarget_specialized_parent(ir: &mut crate::ir::IrFile, expression: ExprId, specialized: u32) {
    let Some(parent) = ir
        .specialized_functions
        .get(&specialized)
        .and_then(|specialization| specialization.parent)
    else {
        return;
    };
    let Some(source_parent) = ir
        .specialized_functions
        .get(&parent)
        .map(|specialization| specialization.source)
    else {
        return;
    };
    let Some(enclosure) = ir.callable_reference_enclosures.get_mut(&expression) else {
        return;
    };
    if *enclosure == crate::ir::IrEnclosure::Lambda(source_parent) {
        *enclosure = crate::ir::IrEnclosure::Lambda(parent);
    }
}

fn specialized_lambda_function(
    ir: &mut crate::ir::IrFile,
    function: u32,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    state: &mut Specialization<'_>,
    parent: Option<u32>,
    inline_captures: &[InlineCapture],
) -> Option<u32> {
    if let Some(done) = state.replacements.get(&function).copied() {
        return Some(done);
    }
    if !state.planned_functions.contains(&function) {
        return None;
    }
    // Only a reified argument is a runtime class. Cloning for an ordinary parameter would turn an
    // erased `as? T` into a check of whatever this call inferred. A use that exists only in a nested
    // implementation still counts: that implementation is a separate function, not an expression child.
    // A lambda whose signature names an anonymous class is copied for the same reason as that class:
    // the capture's class is a different one at each call. An exact recorded inline-lambda capture
    // is copied because its body is substituted into this implementation below.
    let uses_reified = function_uses_binding(
        ir,
        function,
        reified_bindings,
        state.decisions,
        &mut HashSet::default(),
        &mut HashSet::default(),
    );
    let captures_anonymous = function_mentions_anonymous_class(ir, function);
    if !uses_reified && !captures_anonymous && inline_captures.is_empty() {
        return None;
    }
    let body = ir
        .functions
        .get(function as usize)
        .and_then(|shape| shape.body)
        .expect("a planned escaping implementation retains its declaration body");
    let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
    let mut copied_expressions = cloned.values().copied().collect::<Vec<_>>();
    copied_expressions.sort_unstable();
    for copy in copied_expressions {
        super::super::specialize_inline_copy(ir, copy, bindings, reified_bindings)
            .expect("a cloned expression id remains valid while a specialization commits");
        // The implementation body is outside the inline template expression DAG. Record the same
        // declaration/source provenance on its copy so a regenerated class can distinguish its
        // declaration lines from the call-site lambda body spliced into it below.
        ir.record_inline_copy_owner(
            copy,
            state.inline_template_function,
            state.inline_template_owner,
            state.inline_template_source,
        );
    }
    let mut shape = ir.functions[function as usize].clone();
    shape.body = Some(cloned_body);
    shape.params = shape
        .params
        .iter()
        .copied()
        .map(|ty| ty_subst_keep_unbound(ty, bindings))
        .collect();
    shape.ret = ty_subst_keep_unbound(shape.ret, bindings);
    if !inline_captures.is_empty() {
        splice_inline_captures(
            ir,
            cloned_body,
            &mut shape,
            inline_captures,
            state.inline_callee_source_name,
        )
        .expect("the mutation-free plan validated every captured-lambda splice");
    }
    // Every source implementation that executes a reified operation is a class, including one
    // nested in another lambda. kotlinc writes that declaration class (`defineNested$1$nested$1`)
    // as well as the specialized copy at the call site. Leaving the nested declaration as an
    // invokedynamic would erase the reified check. A lambda that captures an anonymous class is a
    // class for the same call-site split, without claiming its body reifies a type parameter.
    if uses_reified {
        ir.runtime_reified_lambda_implementations.insert(function);
    }
    if captures_anonymous {
        ir.inline_anonymous_lambdas.insert(function);
    }
    let specialized = crate::ir::clone_function_implementation(
        ir,
        function,
        shape,
        bindings,
        crate::ir::IrSpecializedFunction {
            source: function,
            caller_declaration: state.caller_declaration,
            caller: state.caller,
            caller_is_default: state.caller_is_default,
            caller_source_name: state.caller_source_name.to_string(),
            inline_callee: state.inline_callee,
            inline_callee_source_name: state.inline_callee_source_name.to_string(),
            parent,
            consumed_inline_capture: !inline_captures.is_empty(),
        },
    );
    if !inline_captures.is_empty() {
        transfer_inlined_capture_facts(ir, specialized, inline_captures);
        for capture in inline_captures {
            let IrExpr::Lambda { impl_fn, .. } = ir.expr(capture.lambda) else {
                unreachable!("the plan validated an exact substituted source lambda")
            };
            state.suppressions.insert(*impl_fn);
        }
    }
    if uses_reified {
        ir.runtime_reified_lambda_implementations
            .insert(specialized);
    }
    if captures_anonymous {
        ir.inline_anonymous_lambdas.insert(specialized);
    }
    for expression in cloned.values() {
        let Some(enclosure) = ir.callable_reference_enclosures.get_mut(expression) else {
            continue;
        };
        *enclosure = match *enclosure {
            crate::ir::IrEnclosure::Function(owner) if owner == function => {
                crate::ir::IrEnclosure::Function(specialized)
            }
            crate::ir::IrEnclosure::Lambda(owner) if owner == function => {
                crate::ir::IrEnclosure::Lambda(specialized)
            }
            other => other,
        };
    }
    // Record the copy before descending so a cycle retargets to this function instead of cloning it again.
    state.replacements.insert(function, specialized);
    let nested = lambda_implementations(ir, cloned_body);
    for (expression, implementation) in nested {
        if !state.planned_functions.contains(&implementation) {
            continue;
        }
        let done = specialized_lambda_function(
            ir,
            implementation,
            bindings,
            reified_bindings,
            state,
            Some(specialized),
            &[],
        )
        .expect("a planned nested escaping implementation commits without declining");
        if let IrExpr::Lambda { impl_fn, .. } = &mut ir.exprs[expression as usize] {
            *impl_fn = done;
        }
    }
    remap_implementation_provenance(ir, cloned_body, state.replacements);
    Some(specialized)
}

/// Splice exact inline-parameter lambdas into a call-site copy of the closure that captured them.
///
/// The surrounding inline expansion already replaced each captured parameter read with the exact
/// source lambda and recorded that replacement in `substituted_inline_lambdas`. This step moves the
/// same checked decision across the escaping closure's function boundary: every use of the
/// captured function parameter must be its direct invocation. Once those invocations hold the
/// lambda body, the function parameter and the value capture disappear together.
fn splice_inline_captures(
    ir: &mut crate::ir::IrFile,
    body: ExprId,
    shape: &mut crate::ir::IrFunction,
    inline_captures: &[InlineCapture],
    inline_callee: &str,
) -> Option<()> {
    let old_arity = shape.params.len();
    assert!(
        shape.param_checks.is_empty() || shape.param_checks.len() == old_arity,
        "parameter checks must be empty or exactly parallel to the pre-specialization parameter shape"
    );
    let mut captures = inline_captures.to_vec();
    captures.sort_unstable_by_key(|capture| capture.parameter);
    captures.dedup_by_key(|capture| capture.parameter);
    if captures.len() != inline_captures.len()
        || captures
            .iter()
            .any(|capture| capture.parameter >= shape.params.len())
    {
        return None;
    }

    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let mut invocation_sets = Vec::<(InlineCapture, Vec<ExprId>)>::new();
    for &capture in &captures {
        let parameter = u32::try_from(capture.parameter).ok()?;
        let reads = expressions
            .iter()
            .copied()
            .filter(|&expression| matches!(ir.expr(expression), IrExpr::GetValue(index) if *index == parameter))
            .collect::<HashSet<_>>();
        let invocations = expressions
            .iter()
            .copied()
            .filter(|&expression| {
                matches!(
                    ir.expr(expression),
                    IrExpr::InvokeFunction { func, .. } if reads.contains(func)
                )
            })
            .collect::<Vec<_>>();
        let invoked_reads = invocations
            .iter()
            .filter_map(|&invocation| match ir.expr(invocation) {
                IrExpr::InvokeFunction { func, .. } => Some(*func),
                _ => None,
            })
            .collect::<HashSet<_>>();
        if reads.is_empty() || reads != invoked_reads {
            return None;
        }
        invocation_sets.push((capture, invocations));
    }

    // Replace the declaration closure's one function-value parameter with the exact capture
    // parameters of the supplied lambda. Every old value slot is then moved into the resulting
    // namespace before the supplied body is copied, so caller slots never leak into an unrelated
    // implementation namespace.
    let mut new_params = Vec::new();
    let mut old_formal_slots = Vec::with_capacity(old_arity);
    let mut transferred_slots = HashMap::<usize, Vec<u32>>::default();
    let mut captures_at = captures.iter().peekable();
    for (parameter, ty) in shape.params.iter().copied().enumerate() {
        let replacement = captures_at
            .peek()
            .filter(|capture| capture.parameter == parameter)
            .copied();
        if let Some(capture) = replacement {
            captures_at.next();
            let IrExpr::Lambda {
                impl_fn,
                captures: transferred,
                ..
            } = ir.expr(capture.lambda)
            else {
                return None;
            };
            let parameter_types = &ir.functions.get(*impl_fn as usize)?.params;
            let capture_types = parameter_types.get(..transferred.len())?;
            let start = u32::try_from(new_params.len()).ok()?;
            old_formal_slots.push(start);
            let mut slots = Vec::with_capacity(capture_types.len());
            for &capture_ty in capture_types {
                slots.push(u32::try_from(new_params.len()).ok()?);
                new_params.push(capture_ty);
            }
            transferred_slots.insert(parameter, slots);
        } else {
            old_formal_slots.push(u32::try_from(new_params.len()).ok()?);
            new_params.push(ty);
        }
    }
    if captures_at.next().is_some() {
        return None;
    }
    let new_arity = u32::try_from(new_params.len()).ok()?;
    let local_count = super::super::super::source_calls::rehome_inline_body_values(
        ir,
        body,
        &old_formal_slots,
        new_arity,
    )?;
    let mut next_temporary = new_arity.checked_add(local_count)?;

    for (capture, invocations) in invocation_sets {
        let slots = transferred_slots.get(&capture.parameter)?;
        for invocation in invocations {
            splice_captured_lambda_invocation(
                ir,
                invocation,
                capture.lambda,
                slots,
                inline_callee,
                &mut next_temporary,
            )?;
        }
    }

    shape.params = new_params;
    shape.param_checks = replaced_parameter_checks(ir, &shape.param_checks, &captures, old_arity)?;
    Some(())
}

fn replaced_parameter_checks(
    ir: &crate::ir::IrFile,
    checks: &[Option<crate::ir::IrParameterCheck>],
    captures: &[InlineCapture],
    old_arity: usize,
) -> Option<Vec<Option<crate::ir::IrParameterCheck>>> {
    if !checks.is_empty() && checks.len() != old_arity {
        return None;
    }
    let mut result = Vec::new();
    let mut captures_at = captures.iter().peekable();
    for parameter in 0..old_arity {
        let replacement = captures_at
            .peek()
            .filter(|capture| capture.parameter == parameter)
            .copied();
        if let Some(capture) = replacement {
            captures_at.next();
            let IrExpr::Lambda {
                impl_fn,
                captures: transferred,
                ..
            } = ir.expr(capture.lambda)
            else {
                return None;
            };
            let implementation = ir.functions.get(*impl_fn as usize)?;
            let source = &implementation.param_checks;
            if !source.is_empty() && source.len() != implementation.params.len() {
                return None;
            }
            if source.is_empty() {
                result.extend(std::iter::repeat_n(None, transferred.len()));
            } else {
                result.extend(source.get(..transferred.len())?.iter().copied());
            }
        } else {
            result.push(checks.get(parameter).copied().unwrap_or(None));
        }
    }
    if captures_at.next().is_some() {
        return None;
    }
    if result.iter().all(Option::is_none) {
        result.clear();
    }
    Some(result)
}

/// Inline one captured source lambda at its invocation inside an escaping implementation copy.
fn splice_captured_lambda_invocation(
    ir: &mut crate::ir::IrFile,
    invocation: ExprId,
    lambda: ExprId,
    capture_slots: &[u32],
    inline_callee: &str,
    next_temporary: &mut u32,
) -> Option<()> {
    let (args, params) = match ir.expr(invocation).clone() {
        IrExpr::InvokeFunction { args, params, .. } => (args, params),
        _ => return None,
    };
    let (impl_fn, captures, inline_body, arity) = match ir.expr(lambda).clone() {
        IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body: Some(inline_body),
            arity,
            ..
        } => (impl_fn, captures, inline_body, arity),
        _ => return None,
    };
    if captures.len() != capture_slots.len() || usize::from(arity) != args.len() {
        return None;
    }
    let parameter_types = ir.functions.get(impl_fn as usize)?.params.clone();
    if parameter_types.len() != captures.len() + args.len() || params.len() != args.len() {
        return None;
    }
    let identities = ir
        .function_parameter_identities(impl_fn)
        .map(<[crate::ir::IrParameterIdentity]>::to_vec);
    let mut declarations = Vec::new();
    let mut formal_slots = capture_slots.to_vec();
    for (parameter, (value, ty)) in args
        .into_iter()
        .zip(parameter_types.into_iter().skip(captures.len()))
        .enumerate()
    {
        let identity_parameter = captures.len() + parameter;
        let source_name = identities
            .as_ref()
            .and_then(|identities| identities.get(identity_parameter))
            .and_then(|identity| identity.source_name.clone());
        let slot = match (ir.expr(value), &source_name) {
            (IrExpr::GetValue(slot), None) => *slot,
            _ => {
                let slot = *next_temporary;
                *next_temporary = next_temporary.checked_add(1)?;
                let declaration = ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: stored_value_ty(ty),
                    init: Some(value),
                    named: source_name.is_some(),
                });
                ir.call_operand_bindings.insert(declaration);
                if let Some(name) = source_name {
                    ir.value_names.insert(declaration, name);
                    ir.set_debug_local_provenance(
                        declaration,
                        crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { depth: 0 },
                    );
                }
                declarations.push(declaration);
                slot
            }
        };
        formal_slots.push(slot);
    }
    let marker = ir.add_expr(IrExpr::InlineFrameMarker);
    ir.value_names.insert(marker, inline_callee.to_owned());
    ir.set_debug_local_provenance(
        marker,
        crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
            implementation: impl_fn,
            depth: 0,
        },
    );
    declarations.push(marker);

    let (spliced, cloned) = crate::ir::clone_expression_dag(ir, inline_body);
    if let (Some(source), Some(origin)) = (
        ir.source_debug.current(),
        ir.lambda_origins.get(&impl_fn).cloned(),
    ) {
        for expression in cloned.values().copied() {
            ir.record_inline_copy_owner(expression, impl_fn, origin.lexical_owner, source);
        }
    }
    let local_base = *next_temporary;
    let local_count = super::super::super::source_calls::rehome_inline_body_values(
        ir,
        spliced,
        &formal_slots,
        local_base,
    )?;
    *next_temporary = local_base.checked_add(local_count)?;
    // The invocation remains the declaration closure's outer frame. The supplied lambda owns a
    // nested copied frame, so its stack and source ownership stay distinct from the declaration.
    let frame = ir.add_expr(IrExpr::Block {
        stmts: declarations,
        value: Some(spliced),
    });
    if let Some(source) = ir.source_debug.current() {
        let owner = ir
            .lambda_origins
            .get(&impl_fn)
            .and_then(|origin| origin.lexical_owner);
        ir.record_inline_copy_owner(frame, impl_fn, owner, source);
    }
    ir.copy_inline_copy_mark(invocation, frame);
    ir.exprs[invocation as usize] = IrExpr::Block {
        stmts: Vec::new(),
        value: Some(frame),
    };
    if let Some(end) = ir.expr_end_lines.get(&lambda).copied() {
        ir.expr_end_lines.insert(invocation, end);
    }
    ir.suspend_calls.remove(&invocation);
    ir.suspend_call_overridden_results.remove(&invocation);
    Some(())
}

#[derive(Clone, Copy)]
struct ParameterTransfer {
    parameter: usize,
    source: crate::ir::FunId,
    count: usize,
}

/// Replace each captured inline-function value with the exact caller captures that now belong to
/// the specialized declaration closure.
fn transfer_inlined_capture_facts(
    ir: &mut crate::ir::IrFile,
    function: u32,
    inline_captures: &[InlineCapture],
) {
    let mut captures = inline_captures.to_vec();
    captures.sort_unstable_by_key(|capture| capture.parameter);
    captures.dedup_by_key(|capture| capture.parameter);
    assert_eq!(captures.len(), inline_captures.len());
    let transfers = captures
        .iter()
        .map(|capture| match ir.expr(capture.lambda) {
            IrExpr::Lambda {
                impl_fn, captures, ..
            } => ParameterTransfer {
                parameter: capture.parameter,
                source: *impl_fn,
                count: captures.len(),
            },
            _ => unreachable!("the plan validated an exact substituted source lambda"),
        })
        .collect::<Vec<_>>();
    let new_arity = ir.functions[function as usize].params.len();
    let old_arity = transfers.iter().fold(new_arity, |arity, transfer| {
        arity
            .checked_add(1)
            .and_then(|arity| arity.checked_sub(transfer.count))
            .expect("transferred captures belong to the specialized parameter shape")
    });
    assert!(transfers
        .iter()
        .all(|transfer| transfer.parameter < old_arity));

    let identities = transfers
        .iter()
        .map(|transfer| {
            let mut source = if transfer.count == 0 {
                Vec::new()
            } else {
                ir.function_parameter_identities(transfer.source)
                    .expect("a transferred lambda publishes exact capture identities")
                    [..transfer.count]
                    .to_vec()
            };
            for identity in &mut source {
                identity.provenance = crate::ir::IrParameterProvenance::InlinedCapture;
            }
            (transfer.parameter, source)
        })
        .collect::<Vec<_>>();
    let captured_receivers = transfers
        .iter()
        .map(|transfer| {
            if transfer.count == 0 {
                return (transfer.parameter, Vec::new());
            }
            let identities = ir
                .function_parameter_identities(transfer.source)
                .expect("a transferred lambda publishes exact capture identities");
            let captured_receivers = ir
                .fn_params
                .get(&transfer.source)
                .map(|parameters| parameters.captured_receivers.as_slice())
                .unwrap_or_default();
            let receivers = identities[..transfer.count]
                .iter()
                .map(|identity| match identity.role {
                    crate::ir::IrParameterRole::CapturedReceiver { ordinal } => {
                        Some(captured_receivers[ordinal as usize].clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            (transfer.parameter, receivers)
        })
        .collect::<Vec<_>>();
    let declared_nullable = source_parameter_values(&ir.fn_param_declared_nullable, &transfers);
    let annotations = source_parameter_values(&ir.fn_param_annotations, &transfers);
    let no_infer = source_parameter_values(&ir.fn_param_no_infer, &transfers);
    let transferred_shared = transfers
        .iter()
        .flat_map(|transfer| {
            (0..transfer.count).filter_map(|ordinal| {
                ir.shared_capture_parameters
                    .get(&(
                        transfer.source,
                        u32::try_from(ordinal).expect("capture ordinal fits u32"),
                    ))
                    .copied()
                    .map(|ty| (transfer.parameter, ordinal, ty))
            })
        })
        .collect::<Vec<_>>();

    if let Some(info) = ir.fn_params.get_mut(&function) {
        let mut receiver_facts = info
            .identities
            .iter()
            .map(|identity| match identity.role {
                crate::ir::IrParameterRole::CapturedReceiver { ordinal } => {
                    Some(info.captured_receivers[ordinal as usize].clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        replace_parameter_values(
            &mut info.identities,
            &identities,
            old_arity,
            "parameter identities",
        );
        replace_parameter_values(
            &mut receiver_facts,
            &captured_receivers,
            old_arity,
            "captured receiver facts",
        );
        if let Some(defaults) = &mut info.defaults {
            if !defaults.is_empty() {
                let inserted = transfers
                    .iter()
                    .map(|transfer| (transfer.parameter, vec![None; transfer.count]))
                    .collect::<Vec<_>>();
                replace_parameter_values(defaults, &inserted, old_arity, "parameter defaults");
            }
        }
        if !info.inline_modifiers.is_empty() {
            let inserted = transfers
                .iter()
                .map(|transfer| {
                    (
                        transfer.parameter,
                        vec![crate::types::InlineParameterModifier::None; transfer.count],
                    )
                })
                .collect::<Vec<_>>();
            replace_parameter_values(
                &mut info.inline_modifiers,
                &inserted,
                old_arity,
                "inline parameter modifiers",
            );
        }
        if !info.expansion_modes.is_empty() {
            let inserted = transfers
                .iter()
                .map(|transfer| {
                    (
                        transfer.parameter,
                        vec![crate::types::InlineExpansionMode::Materialize; transfer.count],
                    )
                })
                .collect::<Vec<_>>();
            replace_parameter_values(
                &mut info.expansion_modes,
                &inserted,
                old_arity,
                "inline expansion modes",
            );
        }
        let mut rebuilt_receivers = Vec::new();
        for (parameter, (identity, receiver)) in
            info.identities.iter_mut().zip(receiver_facts).enumerate()
        {
            match &mut identity.role {
                crate::ir::IrParameterRole::CapturedValue { ordinal, .. } => {
                    *ordinal = u32::try_from(parameter).expect("lambda capture ordinal fits u32");
                }
                crate::ir::IrParameterRole::CapturedReceiver { ordinal } => {
                    *ordinal = u32::try_from(rebuilt_receivers.len())
                        .expect("captured receiver ordinal fits u32");
                    rebuilt_receivers.push(
                        receiver.expect("a captured receiver identity keeps its semantic fact"),
                    );
                }
                _ => debug_assert!(receiver.is_none()),
            }
        }
        info.captured_receivers = rebuilt_receivers;
    }
    replace_parallel_map(
        &mut ir.fn_param_declared_nullable,
        function,
        &declared_nullable,
        old_arity,
    );
    replace_parallel_map(
        &mut ir.fn_param_annotations,
        function,
        &annotations,
        old_arity,
    );
    replace_parallel_map(&mut ir.fn_param_no_infer, function, &no_infer, old_arity);
    if let Some(own_from) = ir.lambda_own_params_from.get_mut(&function) {
        assert!(*own_from as usize <= old_arity);
        let shift = transfers
            .iter()
            .filter(|transfer| transfer.parameter < *own_from as usize)
            .fold(0_isize, |shift, transfer| {
                shift + transfer.count as isize - 1
            });
        *own_from = u32::try_from(*own_from as isize + shift)
            .expect("transferred captures keep the lambda capture boundary non-negative");
    }
    let mut remove_vararg = false;
    if let Some(vararg) = ir.fn_varargs.get_mut(&function) {
        assert!(vararg.index < old_arity);
        for transfer in &transfers {
            if vararg.index == transfer.parameter {
                remove_vararg = true;
                break;
            }
            if vararg.index > transfer.parameter {
                vararg.index = usize::try_from(vararg.index as isize + transfer.count as isize - 1)
                    .expect("transferred captures keep a later vararg ordinal non-negative");
            }
        }
    }
    if remove_vararg {
        ir.fn_varargs.remove(&function);
    }

    let old_shared = ir
        .shared_capture_parameters
        .iter()
        .filter(|((owner, _), _)| *owner == function)
        .map(|((_, ordinal), ty)| (*ordinal as usize, *ty))
        .collect::<Vec<_>>();
    ir.shared_capture_parameters
        .retain(|(owner, _), _| *owner != function);
    for (ordinal, ty) in old_shared {
        if transfers
            .binary_search_by_key(&ordinal, |transfer| transfer.parameter)
            .is_ok()
        {
            continue;
        }
        let shifted = shifted_parameter(ordinal, &transfers);
        ir.shared_capture_parameters.insert(
            (
                function,
                u32::try_from(shifted).expect("capture ordinal fits u32"),
            ),
            ty,
        );
    }
    for (parameter, ordinal, ty) in transferred_shared {
        let start = shifted_parameter(parameter, &transfers);
        ir.shared_capture_parameters.insert(
            (
                function,
                u32::try_from(start + ordinal).expect("capture ordinal fits u32"),
            ),
            ty,
        );
    }
}

fn shifted_parameter(parameter: usize, transfers: &[ParameterTransfer]) -> usize {
    let shift = transfers
        .iter()
        .filter(|transfer| transfer.parameter < parameter)
        .fold(0_isize, |shift, transfer| {
            shift + transfer.count as isize - 1
        });
    usize::try_from(parameter as isize + shift)
        .expect("transferred captures keep parameter ordinals non-negative")
}

fn source_parameter_values<T: Clone + Default>(
    facts: &HashMap<u32, Vec<T>>,
    transfers: &[ParameterTransfer],
) -> Vec<(usize, Vec<T>)> {
    transfers
        .iter()
        .map(|transfer| {
            let values = facts
                .get(&transfer.source)
                .map(|values| values[..transfer.count].to_vec())
                .unwrap_or_else(|| vec![T::default(); transfer.count]);
            (transfer.parameter, values)
        })
        .collect()
}

fn replace_parallel_map<T: Clone>(
    facts: &mut HashMap<u32, Vec<T>>,
    function: u32,
    replacements: &[(usize, Vec<T>)],
    old_arity: usize,
) {
    if let Some(values) = facts.get_mut(&function) {
        if values.is_empty() {
            return;
        }
        replace_parameter_values(values, replacements, old_arity, "parameter fact sidecar");
    }
}

fn replace_parameter_values<T: Clone>(
    values: &mut Vec<T>,
    replacements: &[(usize, Vec<T>)],
    old_arity: usize,
    fact: &str,
) {
    assert_eq!(
        values.len(),
        old_arity,
        "{fact} must be exactly parallel to the pre-specialization parameter shape"
    );
    for (parameter, replacement) in replacements.iter().rev() {
        values.splice(*parameter..=*parameter, replacement.iter().cloned());
    }
}

fn function_mentions_anonymous_class(ir: &crate::ir::IrFile, function: u32) -> bool {
    let Some(shape) = ir.functions.get(function as usize) else {
        return false;
    };
    shape
        .params
        .iter()
        .copied()
        .chain(std::iter::once(shape.ret))
        .any(|ty| type_mentions_anonymous_class(ir, ty))
        || ir.exprs.iter().enumerate().any(|(index, expr)| {
            matches!(expr, IrExpr::Lambda { impl_fn, .. } if *impl_fn == function)
                && ir
                    .logical_types
                    .get(&(index as u32))
                    .is_some_and(|ty| type_mentions_anonymous_class(ir, *ty))
        })
}

fn type_mentions_anonymous_class(ir: &crate::ir::IrFile, ty: Ty) -> bool {
    match ty {
        Ty::Obj(name, arguments) => {
            ir.classes
                .iter()
                .any(|class| class.is_anonymous_object && class.fq_name == name)
                || arguments
                    .iter()
                    .any(|argument| type_mentions_anonymous_class(ir, *argument))
        }
        Ty::Fun(signature) => signature
            .params
            .iter()
            .copied()
            .chain(std::iter::once(signature.ret))
            .any(|part| type_mentions_anonymous_class(ir, part)),
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::DefinitelyNotNull(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::StarProjection(inner)
        | Ty::TyParam(_, inner) => type_mentions_anonymous_class(ir, *inner),
        Ty::Intersection(parts) => parts
            .iter()
            .any(|part| type_mentions_anonymous_class(ir, *part)),
        Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => false,
    }
}

#[cfg(test)]
mod tests;

fn function_uses_binding(
    ir: &crate::ir::IrFile,
    function: u32,
    bindings: &HashMap<String, Ty>,
    decisions: &mut HashMap<u32, bool>,
    functions: &mut HashSet<u32>,
    plans: &mut HashSet<u32>,
) -> bool {
    if bindings.is_empty() {
        return false;
    }
    if let Some(done) = decisions.get(&function).copied() {
        return done;
    }
    if !functions.insert(function) {
        return false;
    }
    let used = ir
        .functions
        .get(function as usize)
        .and_then(|function| function.body)
        .is_some_and(|body| dag_uses_binding(ir, body, bindings, functions, decisions, plans));
    decisions.insert(function, used);
    used
}

fn dag_uses_binding(
    ir: &crate::ir::IrFile,
    root: ExprId,
    bindings: &HashMap<String, Ty>,
    functions: &mut HashSet<u32>,
    decisions: &mut HashMap<u32, bool>,
    plans: &mut HashSet<u32>,
) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if node_uses_binding(ir.expr(expression), bindings)
            || expression_facts_use_binding(ir, expression, bindings)
        {
            return true;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            if function_uses_binding(ir, *impl_fn, bindings, decisions, functions, plans) {
                return true;
            }
        }
        if let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) {
            if local_delegate_plan_uses_binding(
                ir,
                access.plan,
                bindings,
                functions,
                decisions,
                plans,
            ) {
                return true;
            }
        }
    }
    false
}

fn local_delegate_plan_uses_binding(
    ir: &crate::ir::IrFile,
    plan: u32,
    bindings: &HashMap<String, Ty>,
    functions: &mut HashSet<u32>,
    decisions: &mut HashMap<u32, bool>,
    plans: &mut HashSet<u32>,
) -> bool {
    if !plans.insert(plan) {
        return false;
    }
    let Some(plan) = ir.local_delegate_plans.get(plan as usize) else {
        return false;
    };
    std::iter::once(plan.getter.body)
        .chain(plan.setter.as_ref().map(|setter| setter.body))
        .any(|body| dag_uses_binding(ir, body, bindings, functions, decisions, plans))
}

/// Markers and inline-lambda receivers name an implementation by [`FunId`]. After a nested
/// implementation is copied, those edges follow the copy. Source class provenance stays on the
/// copied [`crate::ir::IrLambdaOrigin`].
fn remap_implementation_provenance(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    replacements: &HashMap<u32, u32>,
) {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut plans = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(provenance) = ir.debug_local_provenance(expression) {
            if let Some(remapped) = remap_provenance(provenance, replacements) {
                ir.set_debug_local_provenance(expression, remapped);
            }
        }
        if let IrExpr::Try { catches, .. } = ir.expr(expression).clone() {
            let mut catches = catches;
            let mut changed = false;
            for catch in &mut catches {
                let Some(binding) = catch.binding.as_mut() else {
                    continue;
                };
                let Some(provenance) = binding.provenance else {
                    continue;
                };
                let Some(remapped) = remap_provenance(provenance, replacements) else {
                    continue;
                };
                binding.provenance = Some(remapped);
                changed = true;
            }
            if changed {
                if let IrExpr::Try { catches: slot, .. } = &mut ir.exprs[expression as usize] {
                    *slot = catches;
                }
            }
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

fn remap_provenance(
    provenance: crate::ir::IrDebugLocalProvenance,
    replacements: &HashMap<u32, u32>,
) -> Option<crate::ir::IrDebugLocalProvenance> {
    match provenance {
        crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver { implementation } => replacements
            .get(&implementation)
            .copied()
            .map(
                |implementation| crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver {
                    implementation,
                },
            ),
        crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
            implementation,
            depth,
        } => replacements
            .get(&implementation)
            .copied()
            .map(
                |implementation| crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
                    implementation,
                    depth,
                },
            ),
        crate::ir::IrDebugLocalProvenance::InlineValue { .. }
        | crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { .. }
        | crate::ir::IrDebugLocalProvenance::InlineLambdaContinuation { .. }
        | crate::ir::IrDebugLocalProvenance::InlineCallableReferenceParameter { .. }
        | crate::ir::IrDebugLocalProvenance::FunctionFrameMarker => None,
    }
}

fn lambda_implementations(ir: &crate::ir::IrFile, root: ExprId) -> Vec<(ExprId, u32)> {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut plans = HashSet::default();
    let mut implementations = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            implementations.push((expression, *impl_fn));
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    implementations
}

fn plan_lambda_implementations(
    ir: &crate::ir::IrFile,
    roots: impl IntoIterator<Item = ExprId>,
) -> Vec<(ExprId, u32)> {
    let mut pending = Vec::new();
    let mut plans = HashSet::default();
    for root in roots {
        enqueue_local_delegate_plan_bodies(ir, root, &mut pending, &mut plans);
    }
    let mut seen = HashSet::default();
    let mut implementations = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::Lambda {
                impl_fn, captures, ..
            } => {
                implementations.push((expression, *impl_fn));
                pending.extend(captures.iter().copied());
            }
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
        enqueue_local_delegate_plan_bodies(ir, expression, &mut pending, &mut plans);
    }
    implementations
}

fn enqueue_local_delegate_plan_bodies(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    pending: &mut Vec<ExprId>,
    plans: &mut HashSet<u32>,
) {
    let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) else {
        return;
    };
    if !plans.insert(access.plan) {
        return;
    }
    let Some(plan) = ir.local_delegate_plans.get(access.plan as usize) else {
        return;
    };
    pending.push(plan.getter.body);
    pending.extend(plan.setter.as_ref().map(|setter| setter.body));
}

fn uses_binding(ty: Ty, bindings: &HashMap<String, Ty>) -> bool {
    ty_subst_keep_unbound(ty, bindings) != ty
}

fn substitution_uses_binding(
    substitution: &IrCheckedSubstitution,
    bindings: &HashMap<String, Ty>,
) -> bool {
    substitution.reified
        && (uses_binding(substitution.value, bindings)
            || substitution
                .additional_bounds
                .iter()
                .copied()
                .any(|bound| uses_binding(bound, bindings)))
}

fn substitutions_use_binding(
    substitutions: &[IrCheckedSubstitution],
    bindings: &HashMap<String, Ty>,
) -> bool {
    substitutions
        .iter()
        .any(|substitution| substitution_uses_binding(substitution, bindings))
}

/// Runtime call facts that carry a reified type argument. Static result/signature/storage facts are
/// deliberately excluded: specializing those types may be necessary once a copy exists, but they
/// do not cause a closure or anonymous-object implementation to execute a reified operation.
///
/// Anonymous objects share this predicate. `typeOf<T>()` stays an external call until the JVM
/// realizes it, and only the reified substitution says the call is a runtime type operation.
pub(in super::super) fn expression_facts_use_binding(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
) -> bool {
    if ir
        .reified_call_subst
        .get(&expression)
        .is_some_and(|substitutions| {
            substitutions
                .iter()
                .any(|(_, ty)| uses_binding(*ty, bindings))
        })
    {
        return true;
    }
    match ir.expr(expression) {
        IrExpr::Call {
            callee: Callee::External { substitutions, .. },
            ..
        } => substitutions_use_binding(substitutions, bindings),
        IrExpr::Checked(operation) => checked_substitutions(operation)
            .is_some_and(|substitutions| substitutions_use_binding(substitutions, bindings)),
        IrExpr::ReifiedTypeOp { name, .. } | IrExpr::ReifiedClassMarker { name, .. } => {
            bindings.contains_key(name)
        }
        _ => false,
    }
}

fn checked_substitutions(operation: &IrCheckedOperation) -> Option<&[IrCheckedSubstitution]> {
    match operation {
        IrCheckedOperation::Call { substitutions, .. }
        | IrCheckedOperation::ConstructorDelegation { substitutions, .. }
        | IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => Some(substitutions),
        IrCheckedOperation::PropertyReference { .. }
        | IrCheckedOperation::ExternalPropertyRead { .. }
        | IrCheckedOperation::ExternalPropertyWrite { .. }
        | IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. }
        | IrCheckedOperation::RangeConstruction { .. }
        | IrCheckedOperation::RangeContains { .. }
        | IrCheckedOperation::IllegalProgressionStep { .. }
        | IrCheckedOperation::RangeLoop { .. } => None,
    }
}

/// Whether this node carries a runtime type operation the reified map would change.
///
/// Static signature slots are not a reason to copy.
/// [`super::super::specialize_typed_expression`] applies
/// the same split when the copy is built, so detection does not clone the expression to compare it.
fn node_uses_binding(expression: &IrExpr, bindings: &HashMap<String, Ty>) -> bool {
    match expression {
        IrExpr::TypeOp { type_operand, .. } => uses_binding(*type_operand, bindings),
        IrExpr::KClassLiteral { classifier, .. } => {
            classifier.is_some_and(|ty| uses_binding(ty, bindings))
        }
        IrExpr::Call {
            callee:
                Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::TypeOf { ty },
                    ..
                },
            ..
        } => uses_binding(*ty, bindings),
        _ => false,
    }
}
