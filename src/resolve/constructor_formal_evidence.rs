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
        let evidence = self.library_constructor_evidence(owner, classifier, bindings);
        self.constructor_formal_evidence.insert(call, evidence);
    }

    /// Diagnose a selected dependency constructor that cannot be committed because access control
    /// rejects it. Its inference result is still source-observable, but an access-rejected library
    /// call must never be published in [`Checker::constructor_formal_evidence`].
    pub(super) fn report_rejected_library_constructor_evidence(
        &mut self,
        call: ExprId,
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> bool {
        let evidence = self.library_constructor_evidence(owner, classifier, bindings);
        self.report_constructor_evidence(call, evidence)
    }

    fn library_constructor_evidence(
        &self,
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> CallEvidence {
        let own = classifier.own_type_parameter_count;
        let formals = classifier.type_params().get(..own).unwrap_or_default();
        let bounds = classifier
            .type_param_bounds()
            .get(..own)
            .unwrap_or_default();
        Self::constructor_evidence(owner, formals, bounds, bindings)
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
        let evidence = Self::constructor_evidence(owner, formals, formal_bounds, bindings);
        self.constructor_formal_evidence.insert(call, evidence);
    }

    fn constructor_evidence(
        owner: TypeName,
        formals: &[String],
        formal_bounds: &[Vec<Ty>],
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> CallEvidence {
        let evidence = crate::symbol_resolver::constructor_formal_evidence(
            owner,
            formals,
            formal_bounds,
            bindings,
        );
        CallEvidence {
            owner,
            formals: formals.iter().cloned().zip(evidence).collect(),
        }
    }

    /// Check one branch of an `if` with an `else` or a `when` with an `else` arm. kotlinc completes
    /// such a conditional as one synthetic call even in statement position, so a sibling branch can
    /// fix a constructor type parameter this branch leaves open (`when (s) { "a" -> build {
    /// set(T()) }; else -> Buildee() }`). Retain that open evidence until every branch has been
    /// checked; a sibling rebinds it (see [`Self::open_constructor_expectation`]) and the normal
    /// completion-root report handles anything still open.
    pub(super) fn joined_branch_result(
        &mut self,
        scope: &CheckerScope<'_>,
        branch: ExprId,
        wanted: Wanted,
        joined: bool,
    ) -> Ty {
        if !joined || wanted.expected.is_some() || wanted.value_required {
            return self.expr_result(scope, branch, wanted.expected, wanted.value_required);
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

    /// The expectation under which a sibling branch fixes this branch constructor's open formals.
    ///
    /// kotlinc completes an `if`/`when` with an `else`, or an elvis, as one synthetic call, so
    /// `if (c) D(1) else D<String>(2)` constrains the first `D`'s `T` through the sibling. The
    /// branch is checked again with the sibling's face of the constructed classifier as its
    /// expectation, exactly as a generic call result is rebound, and that check's own selection
    /// records the evidence. A sibling that is itself an open constructor supplies nothing (see
    /// [`Self::branch_sibling_type`]).
    pub(super) fn open_constructor_expectation(&self, branch: ExprId, sibling: Ty) -> Option<Ty> {
        let call = conditional_branch::branch_value_expression(self.file, branch);
        let evidence = self.constructor_formal_evidence.get(&call)?;
        if !evidence
            .formals
            .iter()
            .any(|(_, evidence)| *evidence == ConstructorFormalEvidence::Unconstrained)
        {
            return None;
        }
        let source = self.fed_source();
        let (_, applied, _) =
            crate::symbol_resolver::applied_hierarchy(&source, sibling.non_null())
                .into_iter()
                .find(|(owner, _, _)| *owner == evidence.owner)?;
        (!applied.mentions_pending()).then_some(applied)
    }

    /// A branch's type as evidence for its siblings. An open constructor's completed result
    /// substitutes declared bounds for the formals nothing constrained; that substitute is not a
    /// constraint, so such a branch offers no type to rebind a sibling against.
    pub(super) fn branch_sibling_type(&self, branch: ExprId, ty: Ty) -> Ty {
        if self.constructor_branch_is_open(branch) {
            Ty::Error
        } else {
            ty
        }
    }

    /// Whether a conditional branch's selected constructor still owns an open classifier formal.
    ///
    /// A completed upper bound is only the call's provisional result. In particular it must not be
    /// fed into an elvis sibling as though it were a constraint supplied by source.
    pub(super) fn constructor_branch_is_open(&self, branch: ExprId) -> bool {
        let call = conditional_branch::branch_value_expression(self.file, branch);
        self.constructor_formal_evidence
            .get(&call)
            .is_some_and(|evidence| {
                evidence
                    .formals
                    .iter()
                    .any(|(_, evidence)| *evidence == ConstructorFormalEvidence::Unconstrained)
            })
    }

    /// Report a completed constructor call whose class type parameter nothing constrained. kotlinc
    /// reports `cannot infer type for type parameter` for `C(1)` with
    /// `class C<T : Comparable<T>>(val n: Int)`. A self type is completed instead (see
    /// [`ConstructorFormalEvidence::SelfType`]).
    ///
    /// Like the generic-call report, this runs only at a completion root (an untyped declaration
    /// initializer or a statement), after any enclosing call has had the chance to complete it.
    pub(super) fn report_uninferred_constructor_type_parameter(&mut self, call: ExprId) -> bool {
        let Some(evidence) = self.constructor_formal_evidence.remove(&call) else {
            return false;
        };
        self.report_constructor_evidence(call, evidence)
    }

    fn report_constructor_evidence(&mut self, call: ExprId, evidence: CallEvidence) -> bool {
        let Some((formal, _)) = evidence
            .formals
            .into_iter()
            .find(|(_, evidence)| *evidence == ConstructorFormalEvidence::Unconstrained)
        else {
            return false;
        };
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
