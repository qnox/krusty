//! What a checked constructor call's inference established about each class type parameter.
//!
//! Constructor selection records, once per check, how each of the constructed classifier's own
//! type parameters was fixed ([`ConstructorFormalEvidence`]): bound by a written type argument, an
//! argument, the expected type or an enclosing call; completed as a self type; or not constrained at
//! all. The `cannot infer type for type parameter` report reads that record. It never reconstructs
//! the evidence from the selected (already specialized) parameters or from the result type.

use super::*;
use crate::symbol_resolver::ConstructorFormalEvidence;

/// The evidence of one constructor call, in the classifier's own type-parameter order.
#[derive(Clone)]
pub(super) struct CallEvidence {
    owner: TypeName,
    formals: Vec<(String, ConstructorFormalEvidence)>,
}

impl Checker<'_> {
    /// Record the evidence of a source constructor call from its inferred declaration-order
    /// `bindings`, where a type parameter nothing bound is still `Ty::Error`.
    pub(super) fn record_source_constructor_evidence(
        &mut self,
        call: ExprId,
        class: &ClassSig,
        bindings: &[Ty],
    ) {
        let bound = class
            .type_params
            .iter()
            .cloned()
            .zip(bindings.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        let bounds = class
            .type_param_bounds
            .iter()
            .map(|bound| {
                (*bound != Ty::Error)
                    .then_some(*bound)
                    .into_iter()
                    .collect()
            })
            .collect::<Vec<_>>();
        self.record_constructor_evidence(
            call,
            class.internal_name(),
            &class.type_params,
            &bounds,
            &bound,
        );
    }

    /// Record the evidence of a dependency constructor call from the selected declaration's
    /// inferred `bindings`. Only the classifier's own type parameters are the call's to infer; the
    /// captured ones of an inner classifier come from its outer receiver.
    pub(super) fn record_library_constructor_evidence(
        &mut self,
        call: ExprId,
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        let own = classifier.own_type_parameter_count;
        let formals = classifier.type_params().get(..own).unwrap_or_default();
        let bounds = classifier
            .type_param_bounds()
            .get(..own)
            .unwrap_or_default();
        self.record_constructor_evidence(call, owner, formals, bounds, bindings);
    }

    /// Record the evidence of a constructor call of `owner` whose own `formals` inferred `bindings`.
    fn record_constructor_evidence(
        &mut self,
        call: ExprId,
        owner: TypeName,
        formals: &[String],
        formal_bounds: &[Vec<Ty>],
        bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        let evidence = crate::symbol_resolver::constructor_formal_evidence(
            owner,
            formals,
            formal_bounds,
            bindings,
        );
        self.constructor_formal_evidence.insert(
            call,
            CallEvidence {
                owner,
                formals: formals.iter().cloned().zip(evidence).collect(),
            },
        );
    }

    /// Check one branch of an `if` with an `else` or a `when` with an `else` arm. kotlinc completes
    /// such a conditional as one synthetic call even in statement position, so a sibling branch can
    /// fix a constructor type parameter this branch leaves open (`when (s) { "a" -> build {
    /// set(T()) }; else -> Buildee() }`). Retain that open evidence until every branch has been
    /// checked; [`Self::join_conditional_constructor_evidence`] consumes only a sibling's proven
    /// application and the normal completion-root report handles anything still open.
    pub(super) fn joined_branch_result(
        &mut self,
        scope: &CheckerScope<'_>,
        branch: ExprId,
        expected: Option<Ty>,
        value_required: bool,
        joined: bool,
    ) -> Ty {
        if !joined || expected.is_some() || value_required {
            return self.expr_result(scope, branch, expected, value_required);
        }
        self.discarded_exprs.insert(branch);
        let ty = self.expr_with_context(scope, branch, false);
        let result = conditional_branch::branch_value_expression(self.file, branch);
        if !self.constructor_formal_evidence.contains_key(&result)
            && self.report_unbound_call_result_type_parameter(scope, result)
        {
            Ty::Error
        } else {
            ty
        }
    }

    /// Join the fixed classifier arguments established by sibling branches into constructor
    /// evidence before the completion-root diagnostic runs.
    ///
    /// Constructor result completion substitutes a declared bound for an unconstrained formal, so
    /// the resulting `D<Any?>` is not itself evidence for another bare `D()` branch. A sibling
    /// constructor can contribute an argument only when its record says that ordinal was actually
    /// fixed. A non-constructor branch contributes its selected semantic result application. This
    /// is deliberately a provenance join, not another lookup or overload-selection path.
    pub(super) fn join_conditional_constructor_evidence(
        &mut self,
        scope: &CheckerScope<'_>,
        branches: impl IntoIterator<Item = (ExprId, Ty)>,
    ) {
        let branches = branches.into_iter().collect::<Vec<_>>();
        let mut newly_bound = Vec::new();
        for (target_branch, _) in &branches {
            let target_call =
                conditional_branch::branch_value_expression(self.file, *target_branch);
            let Some(target) = self.constructor_formal_evidence.get(&target_call).cloned() else {
                continue;
            };
            for (ordinal, (_, evidence)) in target.formals.iter().enumerate() {
                if *evidence != ConstructorFormalEvidence::Unconstrained {
                    continue;
                }
                let sibling_bound = branches.iter().any(|(sibling_branch, sibling_ty)| {
                    let sibling_call =
                        conditional_branch::branch_value_expression(self.file, *sibling_branch);
                    if sibling_call == target_call {
                        return false;
                    }
                    let sibling_evidence = self.constructor_formal_evidence.get(&sibling_call);
                    if sibling_evidence.is_some_and(|sibling| {
                        if sibling.owner == target.owner {
                            sibling.formals.get(ordinal).is_some_and(|(_, evidence)| {
                                *evidence == ConstructorFormalEvidence::Unconstrained
                            })
                        } else {
                            // Without the derived classifier's declaration substitution, an open
                            // formal cannot be mapped soundly onto the target owner's ordinal.
                            sibling.formals.iter().any(|(_, evidence)| {
                                *evidence == ConstructorFormalEvidence::Unconstrained
                            })
                        }
                    }) {
                        return false;
                    }
                    let source = self.fed_source();
                    crate::symbol_resolver::applied_hierarchy(&source, *sibling_ty)
                        .into_iter()
                        .find(|(owner, _, _)| *owner == target.owner)
                        .and_then(|(_, applied, _)| applied.type_args().get(ordinal).copied())
                        .is_some_and(|argument| {
                            argument != Ty::Error
                                && !argument.mentions_pending()
                                && Self::type_is_lexically_fixed(scope, argument)
                        })
                });
                if sibling_bound {
                    newly_bound.push((target_call, ordinal));
                }
            }
        }
        for (call, ordinal) in newly_bound {
            if let Some((_, evidence)) = self
                .constructor_formal_evidence
                .get_mut(&call)
                .and_then(|evidence| evidence.formals.get_mut(ordinal))
            {
                *evidence = ConstructorFormalEvidence::Bound;
            }
        }
    }

    /// Report a completed constructor call whose class type parameter nothing constrained. kotlinc
    /// reports `cannot infer type for type parameter` for `C(1)` with
    /// `class C<T : Comparable<T>>(val n: Int)`. A self type is completed instead (see
    /// [`ConstructorFormalEvidence::SelfType`]).
    ///
    /// Like the generic-call report, this runs only at a completion root (an untyped declaration
    /// initializer or a statement), after any enclosing call has had the chance to complete it.
    pub(super) fn report_uninferred_constructor_type_parameter(&mut self, call: ExprId) -> bool {
        let Some(formal) = self
            .constructor_formal_evidence
            .get(&call)
            .and_then(|evidence| {
                evidence
                    .formals
                    .iter()
                    .find(|(_, evidence)| *evidence == ConstructorFormalEvidence::Unconstrained)
                    .map(|(formal, _)| formal.clone())
            })
        else {
            return false;
        };
        self.constructor_formal_evidence.remove(&call);
        let message = format!(
            "cannot infer type for type parameter '{}'. Specify it explicitly.",
            crate::types::type_parameter_source_name(&formal)
        );
        self.diags.error(self.call_callee_name_span(call), message);
        true
    }
}

/// Complete a source constructor's declaration-order `bindings`: a `null`-only binding is
/// `Nothing?`, and a type parameter nothing bound takes its declared bound (recorded as
/// [`ConstructorFormalEvidence::Unconstrained`] unless it is a self type).
pub(super) fn complete_source_constructor_bindings(
    class: &ClassSig,
    mut bindings: Vec<Ty>,
) -> Vec<Ty> {
    for (index, binding) in bindings.iter_mut().enumerate() {
        if *binding == Ty::Null {
            *binding = Ty::nullable(Ty::Nothing);
        } else if *binding == Ty::Error {
            *binding = class
                .type_param_bounds
                .get(index)
                .copied()
                .filter(|bound| *bound != Ty::Error)
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        }
    }
    bindings
}
