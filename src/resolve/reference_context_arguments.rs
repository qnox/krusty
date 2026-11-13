//! Context arguments of a callable reference (`CallableReferencesToContextual`).
//!
//! A reference to a declaration with context parameters does not expose them: the checker selects
//! a scope value for each at the reference site, exactly as a call would, and binds the result into
//! the reference. The selection is published through the same `context_args` handoff a call uses,
//! so checked FIR and common lowering never search the scope again.

use super::*;

impl Checker<'_> {
    pub(super) fn callable_ref_function_type(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        expected: Option<Ty>,
        receiver: Option<ExprId>,
        name: String,
    ) -> Ty {
        let ty = self.callable_ref_selected_type(scope, e, expected, receiver, name);
        self.bind_reference_context_arguments(scope, e, ty)
    }

    /// What a reference is selected against when it has no expected function type and its sole
    /// member candidate has context parameters: the shape of the candidate's remaining
    /// parameters, since the reference binds the contexts at its own site. Any other reference
    /// keeps its expectation.
    pub(super) fn natural_reference_expectation(
        &self,
        expected: Option<Ty>,
        candidates: &CallableRefCandidates,
        unbound_receiver: Option<Ty>,
    ) -> Option<Ty> {
        if expected.is_some() || !self.file.callable_references_to_contextual {
            return expected;
        }
        let member = candidates.method.as_ref()?;
        if member.context_count == 0 || candidates.property.is_some() {
            return expected;
        }
        let parameters = member.params.get(member.context_count..)?;
        let parameters = unbound_receiver
            .into_iter()
            .chain(parameters.iter().copied())
            .collect::<Vec<_>>();
        Some(if member.suspend() {
            Ty::fun_suspend(parameters, member.ret)
        } else {
            Ty::fun(parameters, member.ret)
        })
    }

    /// A reference without an expected type to a top-level function with context parameters: the
    /// reference's type is the function's remaining parameters, selected like any adapted reference.
    pub(super) fn contextual_top_level_reference(
        &mut self,
        e: ExprId,
        name: &str,
        selected: &crate::libraries::FunctionInfo,
    ) -> Option<Ty> {
        if selected.generic_sig.is_some() {
            return None;
        }
        let parameters = selected
            .callable
            .params
            .get(selected.context_count..)?
            .to_vec();
        let ty = if selected.callable.suspend {
            Ty::fun_suspend(parameters, selected.callable.ret)
        } else {
            Ty::fun(parameters, selected.callable.ret)
        };
        let TyKind::Fun(expected) = ty.kind() else {
            return None;
        };
        let plan = self.callable_ref_argument_plan(selected, expected)?;
        Some(self.record_adapted_top_level_function_ref(
            e,
            name,
            selected,
            expected,
            None,
            Some(plan),
        ))
    }

    /// How many leading context parameters a reference to `function` fills from its own site.
    /// Without `CallableReferencesToContextual` they stay ordinary leading inputs of the
    /// reference's function type.
    pub(super) fn reference_bound_context_count(
        &self,
        function: &crate::libraries::FunctionInfo,
    ) -> usize {
        if self.file.callable_references_to_contextual {
            function.context_count
        } else {
            0
        }
    }

    /// Without `CallableReferencesToContextual` a declaration with context parameters cannot be
    /// referenced at all.
    pub(super) fn report_context_parameter_reference(
        &mut self,
        expression: ExprId,
        name: &str,
        function: &crate::libraries::FunctionInfo,
    ) -> Ty {
        self.diags.error(
            self.member_name_span(expression, name),
            format!(
                "callable reference to '{}' is unsupported because it has context parameters.",
                self.source_callable_display(function)
                    .unwrap_or_else(|| Self::callable_candidate_display(name, function))
            ),
        );
        Ty::Error
    }

    /// Select and record the reference-site context arguments of the declaration `e` now resolves
    /// to. A reference whose adaptation plan has no leading `Context` entry binds nothing.
    pub(super) fn bind_reference_context_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        ty: Ty,
    ) -> Ty {
        if ty == Ty::Error || !self.file.callable_references_to_contextual {
            return ty;
        }
        let Some(context_types) = self.reference_context_types(e) else {
            self.context_args.remove(&e);
            return ty;
        };
        match self.select_context_arguments_with_types(scope, &context_types) {
            Ok(arguments) => {
                self.context_args.insert(
                    e,
                    arguments
                        .into_iter()
                        .map(|(argument, _)| argument)
                        .collect(),
                );
                ty
            }
            Err(missing) => {
                self.diags.error(
                    self.reference_context_span(e),
                    format!("no context argument for '{}' found.", missing.display(&[])),
                );
                self.set(e, Ty::Error);
                Ty::Error
            }
        }
    }

    fn reference_context_span(&self, e: ExprId) -> Span {
        match self.file.expr(e) {
            Expr::CallableRef { name, .. } => self.member_name_span(e, name),
            _ => self.span(e),
        }
    }

    /// The declared types of the leading context parameters the recorded adaptation plan fills from
    /// the reference site, or `None` when it fills none.
    fn reference_context_types(&self, e: ExprId) -> Option<Vec<Ty>> {
        let (parameters, plan): (&[Ty], &[AdaptedRefArgument]) = match self.expr_lowers.get(&e)? {
            ExprLowering::AdaptedRef {
                target,
                resolved_argument_mapping,
                ..
            } => (&target.params, resolved_argument_mapping),
            ExprLowering::AdaptedCallableReference {
                target,
                argument_mapping,
                ..
            } => {
                let parameters = match target {
                    CallableReferenceTarget::Member { member, .. } => &member.params,
                    CallableReferenceTarget::Classifier(member) => &member.params,
                    CallableReferenceTarget::Extension { callable, .. } => &callable.params,
                    CallableReferenceTarget::ClassifierProperty { .. }
                    | CallableReferenceTarget::Property(_)
                    | CallableReferenceTarget::TopLevelProperty(_) => return None,
                };
                (parameters, argument_mapping)
            }
            _ => return None,
        };
        let count = plan
            .iter()
            .take_while(|argument| matches!(argument, AdaptedRefArgument::Context))
            .count();
        (count > 0).then(|| parameters.get(..count).map(<[Ty]>::to_vec))?
    }
}
