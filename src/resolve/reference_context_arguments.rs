//! Context arguments of a callable reference (`CallableReferencesToContextual`).
//!
//! A reference to a declaration with context parameters does not expose them: the checker selects
//! a scope value for each at the reference site, exactly as a call would, and binds the result into
//! the reference. The selection is published through the same `context_args` handoff a call uses,
//! so checked FIR and common lowering never search the scope again.

use super::*;

impl Checker<'_> {
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
