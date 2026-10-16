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
pub(in crate::fir_lower::inlining) struct EscapingLambdaPlan {
    roots: Vec<PlannedRoot>,
    functions: HashSet<crate::ir::FunId>,
}

struct PlannedRoot {
    copy: ExprId,
    implementation: crate::ir::FunId,
    inline_captures: Vec<InlineCapture>,
}

/// Validate every escaping lambda specialization without changing common IR.
pub(in crate::fir_lower::inlining) fn plan(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
    protected: &HashSet<ExprId>,
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
        // A substituted lambda that owns caller captures stays a function value. The exact
        // declaration lambda whose anonymous-object constructor owns a substituted capture is also
        // handled there: anonymous specialization either retains the supplied closure or consumes
        // a capture-free one in the copied methods. Only a capture-free substitution outside that
        // ownership edge can enter declaration-closure transfer.
        if (substituted_inline_lambdas.contains(&copy)
            && matches!(ir.expr(copy), IrExpr::Lambda { captures, .. } if !captures.is_empty()))
            || anonymous_owned_lambdas.contains(&copy)
        {
            continue;
        }
        let IrExpr::Lambda {
            impl_fn, captures, ..
        } = ir.expr(copy)
        else {
            continue;
        };
        let inline_captures = captures
            .iter()
            .enumerate()
            .filter(|(_, capture)| {
                substituted_inline_lambdas.contains(capture)
                    && !anonymous_owned_substitution_edges.contains(&(copy, **capture))
            })
            .map(|(parameter, &lambda)| InlineCapture { parameter, lambda })
            .collect::<Vec<_>>();
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

/// Commit a previously validated plan after every fallible transformation of the containing
/// expansion has succeeded.
pub(in crate::fir_lower::inlining) fn apply(
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
        // The copied lambda's `inline_body` is a separate expression dag from the implementation.
        // Nested lambdas there still name the declaration implementations until this retarget.
        retarget_lambdas(ir, copy, state.replacements);
        if !inline_captures.is_empty() {
            let IrExpr::Lambda { captures, .. } = &mut ir.exprs[copy as usize] else {
                unreachable!("the specialized escaping value remains a lambda");
            };
            for capture in inline_captures.iter().rev() {
                captures.remove(capture.parameter);
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
    shape: &crate::ir::IrFunction,
    inline_captures: &[InlineCapture],
) -> bool {
    if !shape.param_checks.is_empty() && shape.param_checks.len() != shape.params.len() {
        return false;
    }
    let mut captures = inline_captures.to_vec();
    captures.sort_unstable_by_key(|capture| capture.parameter);
    captures.dedup_by_key(|capture| capture.parameter);
    if captures.len() != inline_captures.len()
        || captures
            .iter()
            .any(|capture| capture.parameter >= shape.params.len())
    {
        return false;
    }
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let Some(mut next_temporary) = u32::try_from(shape.params.len()).ok() else {
        return false;
    };
    for &expression in &expressions {
        for index in super::super::lambda_splice_contract::value_indices(ir.expr(expression)) {
            let Some(next) = index.checked_add(1) else {
                return false;
            };
            next_temporary = next_temporary.max(next);
        }
    }
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
            return false;
        }
        for invocation in invocations {
            if !captured_invocation_is_valid(ir, invocation, capture.lambda, &mut next_temporary) {
                return false;
            }
        }
    }
    true
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
    if !captures.is_empty() || usize::from(*arity) != args.len() || params.len() != args.len() {
        return false;
    }
    let Some(parameter_types) = ir
        .functions
        .get(*impl_fn as usize)
        .map(|function| &function.params)
    else {
        return false;
    };
    if parameter_types.len() != args.len() {
        return false;
    }
    let identities = ir.function_parameter_identities(*impl_fn);
    for (parameter, &value) in args.iter().enumerate() {
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
    let formal_count = match u32::try_from(args.len()) {
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
        },
    );
    if !inline_captures.is_empty() {
        remove_inlined_capture_facts(ir, specialized, inline_captures);
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
    let mut next_temporary = u32::try_from(shape.params.len()).ok()?;
    for &expression in &expressions {
        for index in super::super::lambda_splice_contract::value_indices(ir.expr(expression)) {
            next_temporary = next_temporary.max(index.checked_add(1)?);
        }
    }

    for capture in &captures {
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
        for invocation in invocations {
            splice_captured_lambda_invocation(
                ir,
                invocation,
                capture.lambda,
                inline_callee,
                &mut next_temporary,
            )?;
        }
    }

    // Remove capture parameters from highest to lowest so each recorded ordinal remains valid.
    for capture in captures.iter().rev() {
        shape.params.remove(capture.parameter);
        let threshold = u32::try_from(capture.parameter).ok()?.checked_add(1)?;
        crate::ir::lower_value_indices(ir, body, threshold, 1);
    }
    remove_optional_parallel_values(
        &mut shape.param_checks,
        &captures
            .iter()
            .map(|capture| capture.parameter)
            .collect::<Vec<_>>(),
        old_arity,
        "parameter checks",
    );
    Some(())
}

/// Inline one captured source lambda at its invocation inside an escaping implementation copy.
fn splice_captured_lambda_invocation(
    ir: &mut crate::ir::IrFile,
    invocation: ExprId,
    lambda: ExprId,
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
    // Captures of the substituted argument have to become captures of the specialized outer
    // closure. That ownership transfer is a separate contract; do not reinterpret caller slots as
    // slots of this implementation.
    if !captures.is_empty() || usize::from(arity) != args.len() {
        return None;
    }
    let parameter_types = ir.functions.get(impl_fn as usize)?.params.clone();
    if parameter_types.len() != args.len() || params.len() != args.len() {
        return None;
    }
    let identities = ir
        .function_parameter_identities(impl_fn)
        .map(<[crate::ir::IrParameterIdentity]>::to_vec);
    let mut declarations = Vec::new();
    let mut formal_slots = Vec::with_capacity(args.len());
    for (parameter, (value, ty)) in args.into_iter().zip(parameter_types).enumerate() {
        let source_name = identities
            .as_ref()
            .and_then(|identities| identities.get(parameter))
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

    let (spliced, _) = crate::ir::clone_expression_dag(ir, inline_body);
    let local_base = *next_temporary;
    let local_count = super::super::super::source_calls::rehome_inline_body_values(
        ir,
        spliced,
        &formal_slots,
        local_base,
    )?;
    *next_temporary = local_base.checked_add(local_count)?;
    ir.exprs[invocation as usize] = IrExpr::Block {
        stmts: declarations,
        value: Some(spliced),
    };
    if let Some(end) = ir.expr_end_lines.get(&lambda).copied() {
        ir.expr_end_lines.insert(invocation, end);
    }
    ir.suspend_calls.remove(&invocation);
    ir.suspend_call_overridden_results.remove(&invocation);
    Some(())
}

/// Remove the captured parameter's declaration-owned sidecars from a specialized implementation.
fn remove_inlined_capture_facts(
    ir: &mut crate::ir::IrFile,
    function: u32,
    inline_captures: &[InlineCapture],
) {
    let old_arity = ir.functions[function as usize].params.len() + inline_captures.len();
    let mut parameters = inline_captures
        .iter()
        .map(|capture| capture.parameter)
        .collect::<Vec<_>>();
    parameters.sort_unstable();
    parameters.dedup();
    assert!(
        parameters.iter().all(|&parameter| parameter < old_arity),
        "an inlined capture ordinal belongs to the pre-specialization parameter shape"
    );
    if let Some(info) = ir.fn_params.get_mut(&function) {
        assert_eq!(
            info.identities.len(),
            old_arity,
            "published parameter identities exactly match the pre-specialization arity"
        );
        for &parameter in parameters.iter().rev() {
            info.identities.remove(parameter);
        }
        if let Some(defaults) = &mut info.defaults {
            remove_optional_parallel_values(defaults, &parameters, old_arity, "parameter defaults");
        }
        remove_optional_parallel_values(
            &mut info.inline_modifiers,
            &parameters,
            old_arity,
            "inline parameter modifiers",
        );
        remove_optional_parallel_values(
            &mut info.expansion_modes,
            &parameters,
            old_arity,
            "inline expansion modes",
        );
        for identity in &mut info.identities {
            if let crate::ir::IrParameterRole::CapturedValue { ordinal, .. } = &mut identity.role {
                let removed_before = parameters
                    .iter()
                    .filter(|&&parameter| parameter < *ordinal as usize)
                    .count();
                *ordinal -= u32::try_from(removed_before).expect("lambda capture count fits u32");
            }
        }
    }
    remove_parallel_parameters(
        &mut ir.fn_param_declared_nullable,
        function,
        &parameters,
        old_arity,
    );
    remove_parallel_parameters(
        &mut ir.fn_param_annotations,
        function,
        &parameters,
        old_arity,
    );
    remove_parallel_parameters(&mut ir.fn_param_no_infer, function, &parameters, old_arity);
    if let Some(own_from) = ir.lambda_own_params_from.get_mut(&function) {
        assert!(
            *own_from as usize <= old_arity,
            "the lambda own-parameter boundary belongs to the pre-specialization shape"
        );
        let removed_before_own = parameters
            .iter()
            .filter(|&&parameter| parameter < *own_from as usize)
            .count();
        *own_from -= u32::try_from(removed_before_own).expect("lambda capture count fits u32");
    }
    let mut remove_vararg = false;
    if let Some(vararg) = ir.fn_varargs.get_mut(&function) {
        assert!(
            vararg.index < old_arity,
            "the vararg ordinal belongs to the pre-specialization parameter shape"
        );
        for &parameter in &parameters {
            if vararg.index == parameter {
                remove_vararg = true;
                break;
            }
            if vararg.index > parameter {
                vararg.index -= 1;
            }
        }
    }
    if remove_vararg {
        ir.fn_varargs.remove(&function);
    }
    let shared = ir
        .shared_capture_parameters
        .iter()
        .filter(|((owner, _), _)| *owner == function)
        .map(|((_, ordinal), ty)| (*ordinal, *ty))
        .collect::<Vec<_>>();
    ir.shared_capture_parameters
        .retain(|(owner, _), _| *owner != function);
    for (ordinal, ty) in shared {
        let ordinal = ordinal as usize;
        assert!(
            ordinal < old_arity,
            "a shared-capture ordinal belongs to the pre-specialization parameter shape"
        );
        if parameters.binary_search(&ordinal).is_ok() {
            continue;
        }
        let removed_before = parameters
            .iter()
            .filter(|&&parameter| parameter < ordinal)
            .count();
        let ordinal = ordinal - removed_before;
        ir.shared_capture_parameters.insert(
            (
                function,
                u32::try_from(ordinal).expect("lambda capture ordinal fits u32"),
            ),
            ty,
        );
    }
}

fn remove_parallel_parameters<T>(
    facts: &mut HashMap<u32, Vec<T>>,
    function: u32,
    parameters: &[usize],
    old_arity: usize,
) {
    if let Some(values) = facts.get_mut(&function) {
        remove_optional_parallel_values(values, parameters, old_arity, "parameter fact sidecar");
    }
}

/// Optional sidecars use one of exactly two shapes: empty (the producer published no fact) or
/// exactly parallel to the function's parameter list. A third shape is corrupt IR and must not be
/// carried through specialization with stale ordinals.
fn remove_optional_parallel_values<T>(
    values: &mut Vec<T>,
    parameters: &[usize],
    old_arity: usize,
    fact: &str,
) {
    assert!(
        values.is_empty() || values.len() == old_arity,
        "{fact} must be empty or exactly parallel to the pre-specialization parameter shape"
    );
    if values.is_empty() {
        return;
    }
    for &parameter in parameters.iter().rev() {
        values.remove(parameter);
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
mod tests {
    use super::*;
    use crate::ir::{
        DeclarationAnnotations, FnParamInfo, IrCapturedDeclaration, IrCapturingCallable,
        IrFunction, IrParameterIdentity, IrParameterRole, IrValueCapture, IrVarargParameter,
    };
    use crate::types::{InlineExpansionMode, InlineParameterModifier};

    const LOCAL_CAPTURE: IrValueCapture = IrValueCapture {
        declaration: IrCapturedDeclaration::Variable,
        capturer: IrCapturingCallable::Lambda,
    };

    fn captured(name: &str, ordinal: u32) -> IrParameterIdentity {
        IrParameterIdentity::captured_value(Some(name.to_string()), ordinal, LOCAL_CAPTURE)
    }

    #[test]
    fn splicing_a_middle_capture_keeps_parameter_checks_parallel() {
        let mut ir = crate::ir::IrFile::default();
        let actual_body = ir.add_expr(IrExpr::UnitInstance);
        let actual_implementation = ir.add_fun(IrFunction {
            name: "actual".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(actual_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let actual = ir.add_expr(IrExpr::Lambda {
            impl_fn: actual_implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(actual_body),
        });
        let captured_read = ir.add_expr(IrExpr::GetValue(1));
        let body = ir.add_expr(IrExpr::InvokeFunction {
            func: captured_read,
            args: Vec::new(),
            params: Vec::new(),
            ret: Ty::Unit,
        });
        let mut shape = IrFunction {
            name: "escaping".to_string(),
            params: vec![Ty::String, Ty::fun(Vec::new(), Ty::Unit), Ty::String],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![
                Some(crate::ir::IrParameterCheck::NonNull),
                None,
                Some(crate::ir::IrParameterCheck::NonNull),
            ],
        };

        assert_eq!(
            splice_inline_captures(
                &mut ir,
                body,
                &mut shape,
                &[InlineCapture {
                    parameter: 1,
                    lambda: actual,
                }],
                "inlineCall",
            ),
            Some(())
        );
        assert_eq!(shape.params, [Ty::String, Ty::String]);
        assert_eq!(
            shape.param_checks,
            [
                Some(crate::ir::IrParameterCheck::NonNull),
                Some(crate::ir::IrParameterCheck::NonNull),
            ]
        );
    }

    #[test]
    fn removing_a_middle_inline_capture_keeps_every_parameter_fact_aligned() {
        let mut ir = crate::ir::IrFile::default();
        // The function shape has already dropped the captured inline parameter when its copied
        // sidecars are rewritten.
        let function = ir.add_fun(IrFunction {
            name: "copy".to_string(),
            params: vec![Ty::Int, Ty::String],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let first_default = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
        let removed_default = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(2)));
        let last_default = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(3)));
        ir.fn_params.insert(
            function,
            FnParamInfo {
                identities: vec![
                    captured("first", 0),
                    captured("removed", 1),
                    captured("last", 2),
                ],
                defaults: Some(vec![
                    Some(first_default),
                    Some(removed_default),
                    Some(last_default),
                ]),
                inline_modifiers: vec![
                    InlineParameterModifier::None,
                    InlineParameterModifier::Noinline,
                    InlineParameterModifier::Crossinline,
                ],
                expansion_modes: vec![
                    InlineExpansionMode::Receiver,
                    InlineExpansionMode::Materialize,
                    InlineExpansionMode::Splice,
                ],
                ..FnParamInfo::default()
            },
        );
        ir.fn_param_declared_nullable
            .insert(function, vec![true, false, true]);
        ir.fn_param_annotations.insert(
            function,
            vec![
                DeclarationAnnotations::default().with_suppressed_source_application(true),
                DeclarationAnnotations::default(),
                DeclarationAnnotations::default(),
            ],
        );
        ir.fn_param_no_infer
            .insert(function, vec![false, true, true]);
        ir.lambda_own_params_from.insert(function, 3);
        ir.fn_varargs.insert(
            function,
            IrVarargParameter {
                index: 2,
                is_last: true,
            },
        );
        ir.shared_capture_parameters.insert((function, 0), Ty::Int);
        ir.shared_capture_parameters
            .insert((function, 1), Ty::Boolean);
        ir.shared_capture_parameters
            .insert((function, 2), Ty::String);

        remove_inlined_capture_facts(
            &mut ir,
            function,
            &[InlineCapture {
                parameter: 1,
                lambda: u32::MAX,
            }],
        );

        let info = &ir.fn_params[&function];
        assert_eq!(
            info.identities
                .iter()
                .map(|identity| (identity.source_name.as_deref(), identity.role))
                .collect::<Vec<_>>(),
            [
                (
                    Some("first"),
                    IrParameterRole::CapturedValue {
                        ordinal: 0,
                        capture: LOCAL_CAPTURE,
                    },
                ),
                (
                    Some("last"),
                    IrParameterRole::CapturedValue {
                        ordinal: 1,
                        capture: LOCAL_CAPTURE,
                    },
                ),
            ]
        );
        assert_eq!(
            info.defaults.as_deref(),
            Some([Some(first_default), Some(last_default)].as_slice())
        );
        assert_eq!(
            info.inline_modifiers,
            [
                InlineParameterModifier::None,
                InlineParameterModifier::Crossinline
            ]
        );
        assert_eq!(
            info.expansion_modes,
            [InlineExpansionMode::Receiver, InlineExpansionMode::Splice]
        );
        assert_eq!(ir.fn_param_declared_nullable[&function], [true, true]);
        assert_eq!(ir.fn_param_annotations[&function].len(), 2);
        assert!(ir.fn_param_annotations[&function][0].has_suppressed_source_application());
        assert!(!ir.fn_param_annotations[&function][1].declares_annotations());
        assert_eq!(ir.fn_param_no_infer[&function], [false, true]);
        assert_eq!(ir.lambda_own_params_from[&function], 2);
        assert_eq!(ir.fn_varargs[&function].index, 1);
        assert_eq!(ir.shared_capture_parameters[&(function, 0)], Ty::Int);
        assert_eq!(ir.shared_capture_parameters[&(function, 1)], Ty::String);
        assert!(!ir.shared_capture_parameters.contains_key(&(function, 2)));
    }

    #[test]
    fn failed_later_root_preflight_leaves_no_specialized_artifacts() {
        let mut ir = crate::ir::IrFile::default();
        let actual_body = ir.add_expr(IrExpr::UnitInstance);
        let actual_implementation = ir.add_fun(IrFunction {
            name: "actual".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            body: Some(actual_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let actual_capture = ir.add_expr(IrExpr::GetValue(0));
        let actual = ir.add_expr(IrExpr::Lambda {
            impl_fn: actual_implementation,
            arity: 0,
            captures: vec![actual_capture],
            sam: None,
            inline_body: Some(actual_body),
        });
        let captured_read = ir.add_expr(IrExpr::GetValue(0));
        let invocation = ir.add_expr(IrExpr::InvokeFunction {
            func: captured_read,
            args: Vec::new(),
            params: Vec::new(),
            ret: Ty::Unit,
        });
        let escaping_implementation = ir.add_fun(IrFunction {
            name: "escaping".to_string(),
            params: vec![Ty::fun(Vec::new(), Ty::Unit)],
            ret: Ty::Unit,
            body: Some(invocation),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let copy = ir.add_expr(IrExpr::Lambda {
            impl_fn: escaping_implementation,
            arity: 0,
            captures: vec![actual],
            sam: None,
            inline_body: None,
        });
        let before = (
            ir.exprs.len(),
            ir.functions.len(),
            ir.classes.len(),
            ir.specialized_functions.len(),
            ir.specialized_expansion_order.len(),
        );

        assert!(plan(
            &ir,
            &[(copy, copy)],
            &HashSet::default(),
            &HashSet::from_iter([actual]),
            &HashSet::default(),
            &HashMap::default(),
        )
        .is_none());
        assert_eq!(
            (
                ir.exprs.len(),
                ir.functions.len(),
                ir.classes.len(),
                ir.specialized_functions.len(),
                ir.specialized_expansion_order.len(),
            ),
            before
        );
        assert_eq!(
            ir.functions[actual_implementation as usize].body,
            Some(actual_body)
        );
        assert_eq!(
            ir.functions[escaping_implementation as usize].body,
            Some(invocation)
        );
        let owned_edges = HashSet::from_iter([(copy, actual)]);
        let captured_plan = plan(
            &ir,
            &[(copy, copy)],
            &HashSet::default(),
            &HashSet::from_iter([actual]),
            &owned_edges,
            &HashMap::default(),
        )
        .expect("anonymous specialization owns the retained constructor capture");
        assert!(captured_plan.roots.is_empty());
        assert!(captured_plan.functions.is_empty());
    }

    #[test]
    fn captured_substituted_lambda_stays_materialized_while_capture_free_copy_specializes() {
        let mut ir = crate::ir::IrFile::default();
        let anonymous = crate::types::type_name("sample/InlineAnonymous");
        let mut anonymous_class = crate::ir::IrClass::synthetic(anonymous);
        anonymous_class.is_anonymous_object = true;
        ir.add_class(anonymous_class);
        let body = ir.add_expr(IrExpr::UnitInstance);
        let implementation = ir.add_fun(IrFunction {
            name: "supplied".to_string(),
            params: vec![Ty::obj_name(anonymous)],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let captured_value = ir.add_expr(IrExpr::GetValue(0));
        let supplied = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: vec![captured_value],
            sam: None,
            inline_body: Some(body),
        });

        let captured_substitution_plan = plan(
            &ir,
            &[(supplied, supplied)],
            &HashSet::default(),
            &HashSet::from_iter([supplied]),
            &HashSet::default(),
            &HashMap::default(),
        )
        .expect("a caller-capturing substituted lambda remains materialized");

        assert!(captured_substitution_plan.roots.is_empty());
        assert!(captured_substitution_plan.functions.is_empty());
        assert_eq!(ir.functions[implementation as usize].body, Some(body));
        let IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body,
            ..
        } = ir.expr(supplied)
        else {
            panic!("the substituted lambda remains a lambda")
        };
        assert_eq!(*impl_fn, implementation);
        assert_eq!(captures, &[captured_value]);
        assert_eq!(*inline_body, Some(body));

        let capture_free = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 1,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(body),
        });
        let plan = plan(
            &ir,
            &[(capture_free, capture_free)],
            &HashSet::default(),
            &HashSet::from_iter([capture_free]),
            &HashSet::default(),
            &HashMap::default(),
        )
        .expect("the capture-free substituted lambda remains transferable");
        assert_eq!(plan.roots.len(), 1);
        assert_eq!(plan.functions, HashSet::from_iter([implementation]));

        let suppressions = apply(
            &mut ir,
            plan,
            &HashMap::default(),
            &HashMap::default(),
            crate::fir::DeclarationId::from_raw(0),
            None,
            false,
            "caller",
            implementation,
            None,
            crate::fir::SourceFileId::from_raw(0),
            crate::fir::CallableId::from_raw(0),
            "inlineCall",
        );
        assert!(suppressions.is_empty());
        assert_eq!(ir.functions[implementation as usize].body, Some(body));
        let IrExpr::Lambda {
            impl_fn: specialized,
            ..
        } = ir.expr(capture_free)
        else {
            panic!("the capture-free substitution remains a lambda")
        };
        assert_ne!(*specialized, implementation);
        assert!(ir.functions[*specialized as usize].body.is_some());
        assert!(!ir.inline_only_fns.contains(&implementation));
        assert!(!ir.inline_only_fns.contains(specialized));
    }

    #[test]
    fn invalid_nested_specialization_invalidates_the_outer_plan() {
        let mut ir = crate::ir::IrFile::default();
        let anonymous = crate::types::type_name("sample/InlineAnonymous");
        let mut anonymous_class = crate::ir::IrClass::synthetic(anonymous);
        anonymous_class.is_anonymous_object = true;
        anonymous_class.ctor_args.push(crate::ir::IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: Ty::fun(Vec::new(), Ty::Unit),
            declared_ty: None,
            is_field: true,
            field_index: Some(0),
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: crate::ir::IrCtorParameterProvenance::Capture,
            capture_identity: None,
        });
        ir.add_class(anonymous_class);

        let nested_implementation = ir.add_fun(IrFunction {
            name: "nested".to_string(),
            params: vec![Ty::obj_name(anonymous)],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let nested = ir.add_expr(IrExpr::Lambda {
            impl_fn: nested_implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        let actual_body = ir.add_expr(IrExpr::UnitInstance);
        let actual_implementation = ir.add_fun(IrFunction {
            name: "actual".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            body: Some(actual_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let actual_capture = ir.add_expr(IrExpr::GetValue(0));
        let actual = ir.add_expr(IrExpr::Lambda {
            impl_fn: actual_implementation,
            arity: 0,
            captures: vec![actual_capture],
            sam: None,
            inline_body: Some(actual_body),
        });
        let captured_read = ir.add_expr(IrExpr::GetValue(0));
        let invocation = ir.add_expr(IrExpr::InvokeFunction {
            func: captured_read,
            args: Vec::new(),
            params: Vec::new(),
            ret: Ty::Unit,
        });
        let outer_body = ir.add_expr(IrExpr::Block {
            stmts: vec![nested],
            value: Some(invocation),
        });
        let outer_implementation = ir.add_fun(IrFunction {
            name: "outer".to_string(),
            params: vec![Ty::fun(Vec::new(), Ty::Unit)],
            ret: Ty::Unit,
            body: Some(outer_body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let copy = ir.add_expr(IrExpr::Lambda {
            impl_fn: outer_implementation,
            arity: 0,
            captures: vec![actual],
            sam: None,
            inline_body: Some(outer_body),
        });
        ir.logical_types.insert(copy, Ty::obj_name(anonymous));
        let construction = ir.add_expr(IrExpr::New {
            internal: anonymous,
            args: vec![copy],
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });

        assert!(plan(
            &ir,
            &[(copy, copy), (construction, construction)],
            &HashSet::default(),
            &HashSet::from_iter([actual]),
            &HashSet::default(),
            &HashMap::default(),
        )
        .is_none());
        assert!(ir.specialized_functions.is_empty());
        assert_eq!(ir.functions[nested_implementation as usize].body, None);
        assert_eq!(
            ir.functions[outer_implementation as usize].body,
            Some(outer_body)
        );

        let owned_edges =
            super::super::super::escaping_anonymous::owned_substituted_lambda_capture_edges(
                &ir,
                [construction],
                &HashSet::from_iter([actual]),
            );
        assert_eq!(owned_edges, HashSet::from_iter([(copy, actual)]));
        let plan = plan(
            &ir,
            &[(copy, copy), (construction, construction)],
            &HashSet::default(),
            &HashSet::from_iter([actual]),
            &owned_edges,
            &HashMap::default(),
        )
        .expect("anonymous specialization owns the nested constructor lambda");
        assert!(plan.roots.is_empty());
        assert!(plan.functions.is_empty());
    }

    #[test]
    fn duplicate_function_requests_require_identical_capture_provenance() {
        let mut ir = crate::ir::IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let actual_implementation = ir.add_fun(IrFunction {
            name: "actual".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(unit),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let first_actual = ir.add_expr(IrExpr::Lambda {
            impl_fn: actual_implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(unit),
        });
        let second_actual = ir.add_expr(IrExpr::Lambda {
            impl_fn: actual_implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(unit),
        });
        let captured_read = ir.add_expr(IrExpr::GetValue(0));
        let invocation = ir.add_expr(IrExpr::InvokeFunction {
            func: captured_read,
            args: Vec::new(),
            params: Vec::new(),
            ret: Ty::Unit,
        });
        let escaping = ir.add_fun(IrFunction {
            name: "escaping".to_string(),
            params: vec![Ty::fun(Vec::new(), Ty::Unit)],
            ret: Ty::Unit,
            body: Some(invocation),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let first_copy = ir.add_expr(IrExpr::Lambda {
            impl_fn: escaping,
            arity: 0,
            captures: vec![first_actual],
            sam: None,
            inline_body: None,
        });
        let same_copy = ir.add_expr(IrExpr::Lambda {
            impl_fn: escaping,
            arity: 0,
            captures: vec![first_actual],
            sam: None,
            inline_body: None,
        });
        let different_copy = ir.add_expr(IrExpr::Lambda {
            impl_fn: escaping,
            arity: 0,
            captures: vec![second_actual],
            sam: None,
            inline_body: None,
        });
        let substituted = HashSet::from_iter([first_actual, second_actual]);

        let exact_request = [InlineCapture {
            parameter: 0,
            lambda: first_actual,
        }];
        let mut decisions = HashMap::default();
        let mut planned = HashSet::default();
        let mut requests = HashMap::default();
        let mut visiting = HashSet::default();
        assert_eq!(
            plan_function(
                &ir,
                escaping,
                first_copy,
                None,
                &HashMap::default(),
                &exact_request,
                &mut decisions,
                &mut planned,
                &mut requests,
                &mut visiting,
            ),
            FunctionPlan::Planned
        );
        assert_eq!(
            plan_function(
                &ir,
                escaping,
                first_copy,
                None,
                &HashMap::default(),
                &exact_request,
                &mut decisions,
                &mut planned,
                &mut requests,
                &mut visiting,
            ),
            FunctionPlan::Planned
        );
        assert!(plan(
            &ir,
            &[(first_copy, first_copy), (same_copy, same_copy)],
            &HashSet::default(),
            &substituted,
            &HashSet::default(),
            &HashMap::default(),
        )
        .is_none());
        assert!(plan(
            &ir,
            &[(first_copy, first_copy), (different_copy, different_copy)],
            &HashSet::default(),
            &substituted,
            &HashSet::default(),
            &HashMap::default(),
        )
        .is_none());
        assert!(ir.specialized_functions.is_empty());
    }
}

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
pub(in crate::fir_lower::inlining) fn expression_facts_use_binding(
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
