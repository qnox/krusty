use std::collections::{HashMap, HashSet};

use crate::ast::{Expr, ExprId, File, StmtId};
use crate::types::Ty;

use super::{Checker, CheckerScope};

/// The lexical label contributed by call syntax to an unlabelled lambda argument. This is source
/// spelling because Kotlin labels the expression as written; callable identity and shape remain
/// resolver-owned and must not be reconstructed from this label.
pub(super) fn call_implicit_lambda_label(file: &File, call: ExprId) -> Option<&str> {
    let Expr::Call { callee, .. } = file.expr(call) else {
        return None;
    };
    match file.expr(*callee) {
        Expr::Name(name) | Expr::Member { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

/// The declaration a checked return leaves. This is the frontend-owned control-flow identity
/// consumed by checked FIR and common lowering; later phases must not recover it from a label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReturnTarget {
    Function,
    Lambda(ExprId),
}

/// Where the lambda's result constraint came from. An open call-inference variable can currently
/// have the same upper-bound type as a source-declared result (`Any`), so the type alone cannot
/// answer whether it is allowed to be fixed by the body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LambdaResultConstraint {
    Open,
    Fixed(Ty),
}

impl LambdaResultConstraint {
    fn expected(self) -> Option<Ty> {
        match self {
            Self::Open => None,
            Self::Fixed(expected) => Some(expected),
        }
    }
}

/// All mutable return state for the body currently checked. Keeping labels, targets, contributed
/// result types, and expected result types together makes entering a nested body an explicit
/// ownership transfer instead of a collection of parallel `Checker` fields.
pub(super) struct LambdaReturnScopes {
    labels: Vec<(String, ExprId)>,
    function_label: Option<String>,
    returned_types: HashMap<ExprId, Ty>,
    expected_types: HashMap<ExprId, Ty>,
    bare_target: ReturnTarget,
    active_chain: Vec<ExprId>,
    /// The root body expression paired with each active lambda. Only that block's trailing
    /// expression can become the lambda result; nested blocks remain ordinary expressions.
    active_bodies: Vec<ExprId>,
    active_constraints: Vec<LambdaResultConstraint>,
    /// Parallel to `active_chain`: whether each active lambda is inlined into the frame around it,
    /// which is kotlinc's `InlineStatus.returnAllowed`. A return may leave only through lambdas
    /// passed to a plain (neither `crossinline` nor `noinline`) parameter of an inline callee.
    active_inlined: Vec<bool>,
    /// The lambda whose individual value returns are being collected, with those returns.
    collected_returns: Option<(ExprId, Vec<Ty>)>,
    /// Valueless and valued exits already bound to each lambda. The key is the lambda expression
    /// the return resolved to, so two literals that share a label do not share an exit set.
    exits: HashMap<ExprId, LambdaExitForms>,
    /// Open lambda results fixed to `Unit` while their owning body was checked once.
    unit_from_exits: HashSet<ExprId>,
}

impl Default for LambdaReturnScopes {
    fn default() -> Self {
        Self {
            labels: Vec::new(),
            function_label: None,
            returned_types: HashMap::new(),
            expected_types: HashMap::new(),
            bare_target: ReturnTarget::Function,
            active_chain: Vec::new(),
            active_bodies: Vec::new(),
            active_constraints: Vec::new(),
            active_inlined: Vec::new(),
            collected_returns: None,
            exits: HashMap::new(),
            unit_from_exits: HashSet::new(),
        }
    }
}

/// The return state of the body around a named function, restored when the function's body ends.
pub(super) struct FunctionReturnFrame {
    label: Option<String>,
    bare_target: ReturnTarget,
    chain: Vec<ExprId>,
    bodies: Vec<ExprId>,
    constraints: Vec<LambdaResultConstraint>,
    inlined: Vec<bool>,
}

pub(super) struct LambdaReturnFrame {
    label_depth: usize,
    chain_depth: usize,
    previous_expected: Option<Ty>,
}

/// Exits bound to one lambda. A valueless exit contributes `Unit`. A valued exit is a real result
/// and is joined with the tail.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LambdaExitForms {
    valueless: bool,
    valued: bool,
}

impl LambdaReturnScopes {
    pub(super) fn target(&self, label: Option<&str>) -> Option<ReturnTarget> {
        match label {
            Some(label) => self
                .labels
                .iter()
                .rev()
                .find_map(|(candidate, lambda)| {
                    (candidate == label).then_some(ReturnTarget::Lambda(*lambda))
                })
                .or_else(|| {
                    (self.function_label.as_deref() == Some(label))
                        .then_some(ReturnTarget::Function)
                }),
            None => Some(self.bare_target),
        }
    }

    pub(super) fn active_labels(&self) -> &[(String, ExprId)] {
        &self.labels
    }

    /// Open the body of a named function (`label`) nested in the current body. Its returns target
    /// the function itself, and no lambda around it is on their way: a return that names one of
    /// those lambdas crosses the function and is prohibited.
    pub(super) fn enter_function(&mut self, label: Option<String>) -> FunctionReturnFrame {
        FunctionReturnFrame {
            label: std::mem::replace(&mut self.function_label, label),
            bare_target: std::mem::replace(&mut self.bare_target, ReturnTarget::Function),
            chain: std::mem::take(&mut self.active_chain),
            bodies: std::mem::take(&mut self.active_bodies),
            constraints: std::mem::take(&mut self.active_constraints),
            inlined: std::mem::take(&mut self.active_inlined),
        }
    }

    pub(super) fn leave_function(&mut self, frame: FunctionReturnFrame) {
        self.function_label = frame.label;
        self.bare_target = frame.bare_target;
        self.active_chain = frame.chain;
        self.active_bodies = frame.bodies;
        self.active_constraints = frame.constraints;
        self.active_inlined = frame.inlined;
    }

    pub(super) fn replace_bare_target(&mut self, target: ReturnTarget) -> ReturnTarget {
        std::mem::replace(&mut self.bare_target, target)
    }

    pub(super) fn enter_lambda(
        &mut self,
        lambda: ExprId,
        body: ExprId,
        label: Option<String>,
        constraint: LambdaResultConstraint,
        inlined: bool,
    ) -> LambdaReturnFrame {
        let frame = LambdaReturnFrame {
            label_depth: self.labels.len(),
            chain_depth: self.active_chain.len(),
            previous_expected: match constraint.expected() {
                Some(expected) => self.expected_types.insert(lambda, expected),
                None => self.expected_types.remove(&lambda),
            },
        };
        self.exits.remove(&lambda);
        self.unit_from_exits.remove(&lambda);
        if let Some(label) = label {
            self.labels.push((label, lambda));
        }
        self.active_chain.push(lambda);
        self.active_bodies.push(body);
        self.active_constraints.push(constraint);
        self.active_inlined.push(inlined);
        self.returned_types.remove(&lambda);
        frame
    }

    pub(super) fn leave_lambda(&mut self, lambda: ExprId, frame: LambdaReturnFrame) {
        match frame.previous_expected {
            Some(expected) => self.expected_types.insert(lambda, expected),
            None => self.expected_types.remove(&lambda),
        };
        self.labels.truncate(frame.label_depth);
        self.active_chain.truncate(frame.chain_depth);
        self.active_bodies.truncate(frame.chain_depth);
        self.active_constraints.truncate(frame.chain_depth);
        self.active_inlined.truncate(frame.chain_depth);
        self.exits.remove(&lambda);
        self.unit_from_exits.remove(&lambda);
    }

    /// Record one resolved exit of `lambda`. `valueless` is a bare `return@label`.
    pub(super) fn record_lambda_exit(&mut self, lambda: ExprId, valueless: bool) {
        let forms = self.exits.entry(lambda).or_default();
        if valueless {
            forms.valueless = true;
        } else {
            forms.valued = true;
        }
    }

    /// Whether `body` is the owning body of the active lambda and semantic return binding has
    /// established that its still-open result has only valueless exits.
    fn open_body_has_only_valueless_exits(&self, body: ExprId) -> Option<ExprId> {
        let lambda = *self.active_chain.last()?;
        if self.active_bodies.last() != Some(&body)
            || self.active_constraints.last() != Some(&LambdaResultConstraint::Open)
        {
            return None;
        }
        self.exits
            .get(&lambda)
            .is_some_and(|forms| forms.valueless && !forms.valued)
            .then_some(lambda)
    }

    fn mark_unit_from_exits(&mut self, lambda: ExprId) {
        self.unit_from_exits.insert(lambda);
    }

    fn take_unit_from_exits(&mut self, lambda: ExprId) -> bool {
        self.unit_from_exits.remove(&lambda)
    }

    /// Whether a return to `target` leaves a frame it may not: a lambda that is not inlined into
    /// the frame around it, or the named function whose body is being checked. kotlinc reports
    /// such a return as `'return' is prohibited here.`: the lambda's body runs in its own frame (a
    /// `noinline` or non-inline argument, or a lambda that is no argument at all) or inside an
    /// object's method (`crossinline`), where the enclosing declaration's frame is gone.
    pub(super) fn leaves_its_frame(&self, target: ReturnTarget) -> bool {
        for (&lambda, &inlined) in self.active_chain.iter().zip(&self.active_inlined).rev() {
            if target == ReturnTarget::Lambda(lambda) {
                return false;
            }
            if !inlined {
                return true;
            }
        }
        // Every lambda of this function's body is behind us, so a lambda target lies outside it.
        matches!(target, ReturnTarget::Lambda(_))
    }

    pub(super) fn expected_type(&self, lambda: ExprId) -> Option<Ty> {
        self.expected_types.get(&lambda).copied()
    }

    fn returned_type(&self, lambda: ExprId) -> Option<Ty> {
        self.returned_types.get(&lambda).copied()
    }

    fn record_returned_type(&mut self, lambda: ExprId, returned: Ty) {
        self.returned_types.insert(lambda, returned);
    }

    pub(super) fn take_returned_type(&mut self, lambda: ExprId) -> Option<Ty> {
        self.returned_types.remove(&lambda)
    }

    pub(super) fn active_chain(&self) -> &[ExprId] {
        &self.active_chain
    }

    /// Start collecting the type of each value `lambda` returns through a labelled `return`,
    /// individually rather than joined.
    pub(super) fn collect_returns(&mut self, lambda: ExprId) {
        self.collected_returns = Some((lambda, Vec::new()));
    }

    /// The returns collected since [`Self::collect_returns`], ending the collection.
    pub(super) fn take_collected_returns(&mut self) -> Vec<Ty> {
        self.collected_returns
            .take()
            .map(|(_, returns)| returns)
            .unwrap_or_default()
    }
}

impl Checker<'_> {
    /// Join every value-return exit through the semantic source as it is recorded. Waiting until the
    /// tail is known is too late: two related labelled returns would already have collapsed to `Any`.
    pub(super) fn record_lambda_returned_type(&mut self, lambda: ExprId, returned: Ty) {
        if let Some((collected, returns)) = &mut self.lambda_returns.collected_returns {
            if *collected == lambda {
                returns.push(returned);
            }
        }
        let merged = match self.lambda_returns.returned_type(lambda) {
            None => returned,
            Some(_) if returned == Ty::Error => Ty::Error,
            Some(Ty::Error) => Ty::Error,
            Some(current) => {
                let source = self.fed_source();
                crate::symbol_resolver::merge_inferred_ty_from_symbols(
                    Some(&source),
                    current,
                    returned,
                )
            }
        };
        self.lambda_returns.record_returned_type(lambda, merged);
    }

    /// The RETURN of the function type a lambda expression carries. An anonymous function's declared
    /// return type (`fun (…): T`) wins over the body type: a block body ending in `return` types as
    /// `Nothing`, which would otherwise erase the result (and make the lowered closure emit a void
    /// `return` where its caller expects a value). Falls back to the body type when undeclared.
    pub(super) fn lambda_ret_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        lambda: ExprId,
        tail: Ty,
        coerce_return_to_unit: bool,
    ) -> Ty {
        match self.file.anon_fun_ret.get(&lambda.0).cloned() {
            Some(declared) => self.type_ref_ty(scope, &declared),
            None if coerce_return_to_unit => Ty::Unit,
            None => match self.lambda_returns.take_returned_type(lambda) {
                None => tail,
                Some(returned) if tail == Ty::Nothing => returned,
                Some(_) if tail == Ty::Error => Ty::Error,
                Some(Ty::Error) => Ty::Error,
                Some(returned) => {
                    let source = self.fed_source();
                    crate::symbol_resolver::merge_inferred_ty_from_symbols(
                        Some(&source),
                        returned,
                        tail,
                    )
                }
            },
        }
    }

    /// Type the lambda body once. Labelled exits are resolved and recorded by that ordinary
    /// checker traversal; the owning block uses those records immediately before checking its
    /// trailing expression.
    pub(super) fn type_lambda_body(
        &mut self,
        scope: &CheckerScope<'_>,
        lambda: ExprId,
        body: ExprId,
        coerce_return_to_unit: bool,
        result_constraint: LambdaResultConstraint,
    ) -> (Ty, bool) {
        let checked = self.check_lambda_body(scope, body, coerce_return_to_unit, result_constraint);
        let unit_from_exits = self.lambda_returns.take_unit_from_exits(lambda);
        (checked, unit_from_exits)
    }

    pub(super) fn record_labelled_lambda_exit(
        &mut self,
        target: ReturnTarget,
        labelled: bool,
        valueless: bool,
    ) {
        if labelled {
            if let ReturnTarget::Lambda(lambda) = target {
                self.lambda_returns.record_lambda_exit(lambda, valueless);
            }
        }
    }

    /// Check the trailing expression of an open lambda as a `Unit` statement once semantic return
    /// binding has established that all exits from this exact lambda are valueless.
    pub(super) fn type_open_lambda_unit_tail(
        &mut self,
        scope: &CheckerScope<'_>,
        body: ExprId,
        expression: ExprId,
    ) -> Option<Ty> {
        let lambda = self
            .lambda_returns
            .open_body_has_only_valueless_exits(body)?;
        self.lambda_returns.mark_unit_from_exits(lambda);
        self.expected = Some(Ty::Unit);
        Some(match self.expr_statement(scope, expression) {
            Ty::Nothing => Ty::Nothing,
            Ty::Error => Ty::Error,
            _ => Ty::Unit,
        })
    }

    /// The lambda's result after its body has been typed. A `Unit` result from valueless exits is
    /// already fixed; completing an expectation-free generic tail as `Nothing` would drop it.
    pub(super) fn lambda_result_type(
        &mut self,
        scope: &CheckerScope<'_>,
        lambda: ExprId,
        body: ExprId,
        tail: Ty,
        coerce_to_unit: bool,
        result_constraint: LambdaResultConstraint,
    ) -> Ty {
        let inferred = self.lambda_ret_ty(scope, lambda, tail, coerce_to_unit);
        let inferred = if !coerce_to_unit
            && result_constraint == LambdaResultConstraint::Open
            && !self.file.anon_fun_ret.contains_key(&lambda.0)
        {
            let result = super::conditional_branch::branch_value_expression(self.file, body);
            self.unbound_contextual_result_signature(result)
                .map(|signature| {
                    crate::symbol_resolver::instantiate_unconstrained_result(&signature, inferred)
                })
                .unwrap_or(inferred)
        } else {
            inferred
        };
        // The body's own inferred result, before coercion to the selected function type's return.
        // A backend that specializes the implementation method's signature (kotlinc's indy
        // metafactory adaptation) reads this, not the caller-facing boundary type.
        self.lambda_body_results.insert(lambda, inferred);
        match result_constraint {
            LambdaResultConstraint::Fixed(expected) => {
                let result = super::conditional_branch::branch_value_expression(self.file, body);
                self.expect_assignable(expected, inferred, self.span(result), "return");
                expected
            }
            LambdaResultConstraint::Open => inferred,
        }
    }

    fn check_lambda_body(
        &mut self,
        scope: &CheckerScope<'_>,
        body: ExprId,
        coerce_return_to_unit: bool,
        result_constraint: LambdaResultConstraint,
    ) -> Ty {
        if coerce_return_to_unit {
            return match self.expr_statement(scope, body) {
                Ty::Nothing => Ty::Nothing,
                Ty::Error => Ty::Error,
                _ => Ty::Unit,
            };
        }
        match result_constraint {
            LambdaResultConstraint::Fixed(expected) => self.expr_declared(scope, body, expected),
            LambdaResultConstraint::Open => self.expr(scope, body),
        }
    }

    /// Install the exact return scope for one lambda while checking its body. The explicit literal
    /// label wins over the call-site's implicit label. Anonymous functions also own bare returns;
    /// ordinary lambdas leave bare returns targeted at the enclosing function.
    pub(super) fn with_lambda_return_scope<R>(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        body: ExprId,
        implicit_label: Option<&str>,
        mut result_constraint: LambdaResultConstraint,
        check: impl FnOnce(&mut Self, LambdaResultConstraint) -> R,
    ) -> R {
        let label = self
            .file
            .lambda_labels
            .get(&e.0)
            .map(String::as_str)
            .or(implicit_label)
            .map(str::to_string);
        // Only a lambda whose selected parameter inlines it runs in the caller's frame. Any other
        // lambda (no call argument, or one no selected inline parameter took) has kotlinc's
        // `InlineStatus.Unknown`, which does not allow a return to leave through it.
        let inlined_argument = self.argument_lambda_inlining.get(&e) == Some(&true);
        if let Some(reference) = self.file.anon_fun_ret.get(&e.0).cloned() {
            result_constraint = LambdaResultConstraint::Fixed(self.type_ref_ty(scope, &reference));
        }
        let frame = self.lambda_returns.enter_lambda(
            e,
            body,
            label.clone(),
            result_constraint,
            inlined_argument,
        );
        crate::trace_compiler!(
            "resolve",
            "lambda return scope enter expression={e:?} label={label:?}"
        );
        let anonymous = self.file.anon_fun_lambdas.contains(&e.0);
        // A lambda is a control-flow boundary — EXCEPT an inlined one. Since Kotlin 2.2
        // (`BreakContinueInInlineLambdas`, default-on at the 2.4 language level krusty targets) a
        // `break`/`continue` inside an inline lambda targets the enclosing loop, because the body is
        // spliced into it. `allow_lambda_mutation` is set from the callee's `is_inline` and brackets
        // this body check, and it means exactly "this body is inlined into the caller's frame" — the
        // same property that makes the jump legal.
        let inlined = self.allow_lambda_mutation;
        let outer_loop_labels = if inlined {
            self.loop_labels.clone()
        } else {
            std::mem::take(&mut self.loop_labels)
        };
        let outer_loop_depth = if inlined {
            self.loop_depth
        } else {
            std::mem::replace(&mut self.loop_depth, 0)
        };
        let saved = anonymous.then(|| {
            let declared = self.file.anon_fun_ret.get(&e.0).cloned();
            let ret = match declared {
                Some(reference) => self.type_ref_ty(scope, &reference),
                None => Ty::Unit,
            };
            let state = (
                self.ret_ty,
                self.return_allowed,
                self.lambda_returns
                    .replace_bare_target(ReturnTarget::Lambda(e)),
            );
            self.ret_ty = ret;
            self.return_allowed = true;
            state
        });
        // Suppressing receiver accounting is scoped to the immediate expression evaluated at an
        // anonymous object's construction site. A non-inline lambda is a later execution/capture
        // boundary: receiver selections in its body must be retained by that closure (and by any
        // parser-hoisted anonymous constructor that carries the closure). An inline lambda remains
        // part of the surrounding expression and therefore keeps the suppression.
        let previous_capture_accounting = (!inlined)
            .then(|| std::mem::replace(&mut self.suppress_receiver_capture_accounting, false));
        let out = check(self, result_constraint);
        if let Some(previous) = previous_capture_accounting {
            self.suppress_receiver_capture_accounting = previous;
        }
        if let Some((ret_ty, return_allowed, bare_return_target)) = saved {
            self.ret_ty = ret_ty;
            self.return_allowed = return_allowed;
            self.lambda_returns.replace_bare_target(bare_return_target);
        }
        self.loop_labels = outer_loop_labels;
        self.loop_depth = outer_loop_depth;
        self.lambda_returns.leave_lambda(e, frame);
        crate::trace_compiler!(
            "resolve",
            "lambda return scope exit expression={e:?} label={label:?}"
        );
        out
    }
}

impl Checker<'_> {
    /// Report a `return@label` whose label denotes no enclosing lambda.
    ///
    /// The reference compiler words this `unresolved label.` and points at the `@` token rather than
    /// the `return` keyword, for an unknown name and for a name whose lambda does not enclose the
    /// return alike. The `@` span is the parser's record; without it the only position available is
    /// the whole statement or expression, which underlines the wrong thing.
    pub(super) fn report_unresolved_statement_label(&mut self, statement: StmtId) {
        let span = self
            .file
            .return_label_spans
            .statement(statement)
            .unwrap_or(self.file.stmt_spans[statement.0 as usize]);
        self.diags.error(span, "unresolved label.".to_string());
    }

    /// The same in expression position (`x ?: return@Missing`), which reaches the checker through a
    /// different site and so carries its own span.
    pub(super) fn report_unresolved_expression_label(&mut self, expression: ExprId) {
        let span = self
            .file
            .return_label_spans
            .expression(expression)
            .unwrap_or(self.span(expression));
        self.diags.error(span, "unresolved label.".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_lambda_scope_restores_targets_and_expectations() {
        let outer = ExprId(1);
        let inner = ExprId(2);
        let mut scopes = LambdaReturnScopes::default();
        let outer_frame = scopes.enter_lambda(
            outer,
            ExprId(11),
            Some("scope".into()),
            LambdaResultConstraint::Fixed(Ty::String),
            true,
        );
        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(outer))
        );
        assert_eq!(scopes.expected_type(outer), Some(Ty::String));

        let inner_frame = scopes.enter_lambda(
            inner,
            ExprId(12),
            Some("scope".into()),
            LambdaResultConstraint::Fixed(Ty::Int),
            true,
        );
        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(inner))
        );
        scopes.leave_lambda(inner, inner_frame);

        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(outer))
        );
        assert_eq!(scopes.expected_type(inner), None);
        scopes.leave_lambda(outer, outer_frame);
        assert_eq!(scopes.target(Some("scope")), None);
        assert_eq!(scopes.expected_type(outer), None);
    }

    #[test]
    fn a_return_may_leave_only_through_inlined_lambdas() {
        let inlined = ExprId(1);
        let stored = ExprId(2);
        let mut scopes = LambdaReturnScopes::default();
        let inlined_frame = scopes.enter_lambda(
            inlined,
            ExprId(11),
            Some("plain".into()),
            LambdaResultConstraint::Open,
            true,
        );
        assert!(!scopes.leaves_its_frame(ReturnTarget::Function));

        let stored_frame = scopes.enter_lambda(
            stored,
            ExprId(12),
            Some("kept".into()),
            LambdaResultConstraint::Open,
            false,
        );
        assert!(scopes.leaves_its_frame(ReturnTarget::Function));
        assert!(scopes.leaves_its_frame(ReturnTarget::Lambda(inlined)));
        assert!(!scopes.leaves_its_frame(ReturnTarget::Lambda(stored)));
        scopes.leave_lambda(stored, stored_frame);

        assert!(!scopes.leaves_its_frame(ReturnTarget::Function));
        scopes.leave_lambda(inlined, inlined_frame);
    }

    #[test]
    fn same_label_exits_follow_the_resolved_lambda_and_do_not_leak() {
        let outer = ExprId(1);
        let inner = ExprId(2);
        let outer_body = ExprId(11);
        let inner_body = ExprId(12);
        let mut scopes = LambdaReturnScopes::default();
        let outer_frame = scopes.enter_lambda(
            outer,
            outer_body,
            Some("label".into()),
            LambdaResultConstraint::Open,
            true,
        );

        let inner_frame = scopes.enter_lambda(
            inner,
            inner_body,
            Some("label".into()),
            LambdaResultConstraint::Open,
            true,
        );
        let ReturnTarget::Lambda(resolved) = scopes.target(Some("label")).unwrap() else {
            panic!("the inner label resolves to a lambda");
        };
        assert_eq!(resolved, inner);
        scopes.record_lambda_exit(resolved, true);
        assert_eq!(
            scopes.open_body_has_only_valueless_exits(inner_body),
            Some(inner)
        );
        assert_eq!(scopes.open_body_has_only_valueless_exits(outer_body), None);
        scopes.record_lambda_exit(outer, false);

        scopes.leave_lambda(inner, inner_frame);
        assert_eq!(scopes.open_body_has_only_valueless_exits(outer_body), None);
        scopes.leave_lambda(outer, outer_frame);
        assert_eq!(scopes.open_body_has_only_valueless_exits(outer_body), None);
    }
}
