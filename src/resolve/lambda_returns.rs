use std::collections::HashMap;

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
    /// Parallel to `active_chain`: whether each active lambda is inlined into the frame around it,
    /// which is kotlinc's `InlineStatus.returnAllowed`. A return may leave only through lambdas
    /// passed to a plain (neither `crossinline` nor `noinline`) parameter of an inline callee.
    active_inlined: Vec<bool>,
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
            active_inlined: Vec::new(),
        }
    }
}

/// The return state of the body around a named function, restored when the function's body ends.
pub(super) struct FunctionReturnFrame {
    label: Option<String>,
    bare_target: ReturnTarget,
    chain: Vec<ExprId>,
    inlined: Vec<bool>,
}

pub(super) struct LambdaReturnFrame {
    label_depth: usize,
    chain_depth: usize,
    previous_expected: Option<Ty>,
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
            inlined: std::mem::take(&mut self.active_inlined),
        }
    }

    pub(super) fn leave_function(&mut self, frame: FunctionReturnFrame) {
        self.function_label = frame.label;
        self.bare_target = frame.bare_target;
        self.active_chain = frame.chain;
        self.active_inlined = frame.inlined;
    }

    pub(super) fn replace_bare_target(&mut self, target: ReturnTarget) -> ReturnTarget {
        std::mem::replace(&mut self.bare_target, target)
    }

    pub(super) fn enter_lambda(
        &mut self,
        lambda: ExprId,
        label: Option<String>,
        expected: Option<Ty>,
        inlined: bool,
    ) -> LambdaReturnFrame {
        let frame = LambdaReturnFrame {
            label_depth: self.labels.len(),
            chain_depth: self.active_chain.len(),
            previous_expected: match expected {
                Some(expected) => self.expected_types.insert(lambda, expected),
                None => self.expected_types.remove(&lambda),
            },
        };
        if let Some(label) = label {
            self.labels.push((label, lambda));
        }
        self.active_chain.push(lambda);
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
        self.active_inlined.truncate(frame.chain_depth);
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
}

impl Checker<'_> {
    /// Join every value-return exit through the semantic source as it is recorded. Waiting until the
    /// tail is known is too late: two related labelled returns would already have collapsed to `Any`.
    pub(super) fn record_lambda_returned_type(&mut self, lambda: ExprId, returned: Ty) {
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
}

impl super::CallLambdaShaping {
    /// Whether the selected callee inlines the lambda for source argument `argument`: from the
    /// selected member's shape, else the extension's, else the provider's expectation.
    fn argument_inlining(&self, argument: usize) -> Option<bool> {
        super::shaped_argument_inlining(self.module.as_ref(), self.extension.as_ref(), argument)
            .or_else(|| {
                self.provider
                    .as_ref()
                    .and_then(|expectations| expectations.get(argument))
                    .and_then(Option::as_ref)
                    .map(|expectation| expectation.inlined)
            })
    }
}

impl Checker<'_> {
    /// Per argument of `receiver.name(args)` where `name` selects a member property rather than a
    /// function, whether the `invoke` operator selected for the property's value inlines that
    /// argument's lambda: the same member, member-extension, extension and provider selection as a
    /// written `value.invoke(args)`. Published before the lambdas are checked, so their return
    /// scopes see the selected operator's parameters. A function-typed value's `invoke` inlines
    /// nothing.
    pub(super) fn property_invoke_argument_inlining(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        receiver: Ty,
        name: &str,
        args_and_partial: (&[ExprId], &[Option<Ty>]),
    ) -> Option<Vec<Option<bool>>> {
        let (args, partial) = args_and_partial;
        if !args
            .iter()
            .any(|argument| matches!(self.file.expr(*argument), Expr::Lambda { .. }))
        {
            return None;
        }
        let selection = self.select_property_read(scope, receiver, name).ok()??;
        let value = self.declared_function_semantic_type(selection.ty());
        if matches!(value.non_null(), Ty::Fun(_)) {
            return None;
        }
        let operator = super::CALLABLE_INVOKE_OPERATOR;
        let (member, generic_member) =
            self.module_member_lambda_plan(scope, call, value, operator, args, partial, None);
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        let shaping = self.call_lambda_shaping(
            scope,
            call,
            value,
            operator,
            args,
            partial,
            arg_names.as_deref(),
            &[],
            None,
            member.as_ref(),
            generic_member.as_ref(),
        );
        Some(
            (0..args.len())
                .map(|argument| shaping.argument_inlining(argument))
                .collect(),
        )
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
        let outer_frame = scopes.enter_lambda(outer, Some("scope".into()), Some(Ty::String), true);
        assert_eq!(
            scopes.target(Some("scope")),
            Some(ReturnTarget::Lambda(outer))
        );
        assert_eq!(scopes.expected_type(outer), Some(Ty::String));

        let inner_frame = scopes.enter_lambda(inner, Some("scope".into()), Some(Ty::Int), true);
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
        let inlined_frame = scopes.enter_lambda(inlined, Some("plain".into()), None, true);
        assert!(!scopes.leaves_its_frame(ReturnTarget::Function));

        let stored_frame = scopes.enter_lambda(stored, Some("kept".into()), None, false);
        assert!(scopes.leaves_its_frame(ReturnTarget::Function));
        assert!(scopes.leaves_its_frame(ReturnTarget::Lambda(inlined)));
        assert!(!scopes.leaves_its_frame(ReturnTarget::Lambda(stored)));
        scopes.leave_lambda(stored, stored_frame);

        assert!(!scopes.leaves_its_frame(ReturnTarget::Function));
        scopes.leave_lambda(inlined, inlined_frame);
    }
}
