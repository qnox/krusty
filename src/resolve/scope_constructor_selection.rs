//! The shared callable selection's decision over a bare call's scope-tower constructors, and its
//! commitment through the provider constructor path. The candidates come from
//! [`scope_constructor_candidates`](super::scope_constructor_candidates); this module never builds
//! a constructor family of its own.

use super::*;

#[cfg(test)]
mod tests;

/// The shared selector's decision over a classifier's scope-tower constructors.
pub(super) enum ScopeConstructorSelection {
    /// One declared constructor applies; only it is materialized.
    Selected(Box<crate::libraries::FunctionInfo>),
    /// Several constructors are equally specific. The provider path reports the ambiguity, but
    /// never commits a constructor of its own choosing.
    Ambiguous(Vec<crate::libraries::FunctionInfo>),
    /// No constructor applies. The provider path reports why, but never commits; what it leaves
    /// unreported is reported over the candidates, as for a function family.
    Inapplicable,
}

impl ScopeConstructorSelection {
    pub(super) fn selected(&self) -> Option<&crate::libraries::FunctionInfo> {
        match self {
            Self::Selected(selected) => Some(selected),
            Self::Ambiguous(_) | Self::Inapplicable => None,
        }
    }

    fn preselection(&self) -> ConstructorPreselection<'_> {
        match self {
            Self::Selected(selected) => ConstructorPreselection::Selected(selected),
            Self::Ambiguous(_) => ConstructorPreselection::Ambiguous,
            Self::Inapplicable => ConstructorPreselection::Rejected,
        }
    }
}

/// The shared selection's decision over a bare call's receiver-less candidates.
pub(super) struct ScopeCallDecision {
    /// The decision the constructor path commits.
    pub(super) constructor: Option<ScopeConstructorSelection>,
    /// A function won against competing constructors, or the nearest rung is ambiguous.
    pub(super) function: Option<CallableCandidateSelection>,
}

/// The shared selection over a bare call's receiver-less functions and the constructors that
/// compete with them on the scope tower.
enum ScopeCallCompetition {
    /// A declared constructor won; the constructor path materializes only it.
    Constructor(Box<crate::libraries::FunctionInfo>),
    /// A function won, or the nearest rung is ambiguous. The function path consumes this decision
    /// and does not select again.
    Callable(CallableCandidateSelection),
}

/// What the shared selection over a bare call's scope constructors reads from the call.
#[derive(Clone, Copy)]
pub(super) struct ScopeConstructorChoiceCall<'a> {
    pub(super) explicit_type_args: &'a [Ty],
    /// The type alias application the call was written through.
    pub(super) alias_target: Option<Ty>,
    pub(super) expected: Option<Ty>,
}

/// The syntax of a bare `Name(args)` constructor call reached on the file/import scope tower.
pub(super) struct ScopeConstructorCall<'a> {
    pub(super) arg_names: Option<&'a [Option<String>]>,
    pub(super) explicit_type_args: &'a [Ty],
    /// The type alias application the call was written through.
    pub(super) alias_target: Option<Ty>,
    pub(super) expected: Option<Ty>,
    pub(super) priority: ConstructorPriorityTier,
    /// The shared selection over `candidates`, when it covers this classifier.
    pub(super) selection: Option<&'a ScopeConstructorSelection>,
    pub(super) candidates: &'a [crate::libraries::FunctionInfo],
}

impl Checker<'_> {
    /// The classifier arguments and result constraint a bare constructor call selects with. A type
    /// alias application fixes the classifier arguments it spells; the ones it leaves open are
    /// inferred like any call's, and the applied type constrains the result.
    fn scope_constructor_constraints(
        &self,
        call_expr: ExprId,
        call: ScopeConstructorChoiceCall<'_>,
        classifier: TypeName,
        type_parameter_count: usize,
    ) -> crate::libraries::ConstructorCallConstraints {
        let applied = self.alias_constructor_fixed_application(call_expr, call.alias_target);
        let type_args = applied
            .filter(|applied| {
                applied.kotlin_class_internal() == Some(classifier)
                    && applied.type_args().len() == type_parameter_count
            })
            .map_or(call.explicit_type_args, |applied| applied.type_args());
        crate::libraries::ConstructorCallConstraints {
            type_arguments: Ty::obj_args_name(classifier, type_args).type_args(),
            expected: Self::alias_constructor_result_constraint(applied, call.expected),
        }
    }

    /// The shared callable selection's decision over a bare call's scope constructors. `None`
    /// leaves the provider path to select: there are no candidates, or the winner is the
    /// functional-interface constructor, which has no declaration to materialize and is committed
    /// by its own conversion path.
    pub(super) fn scope_constructor_choice(
        &self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        call: ScopeConstructorChoiceCall<'_>,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> Option<ScopeConstructorSelection> {
        let classifier = candidates
            .first()?
            .constructor
            .map(|constructor| constructor.classifier)?;
        // A captured type parameter is bound from the call's lexical scope, which only the
        // provider path does.
        let shape = self.resolved_type_name(classifier)?;
        if shape.own_type_parameter_count != shape.type_params().len() {
            return None;
        }
        let constraints = self.scope_constructor_constraints(
            call_args.call,
            call,
            classifier,
            shape.type_params().len(),
        );
        let declared = |candidate: &crate::libraries::FunctionInfo| {
            candidate.constructor.map(|constructor| constructor.origin)
                == Some(crate::libraries::ConstructorOrigin::Declared)
        };
        match self.select_callable_candidate(
            scope,
            call_args,
            call.explicit_type_args,
            None,
            CallResultConstraint::direct(call.expected),
            candidates
                .iter()
                .cloned()
                .map(|candidate| candidate.with_constructor_call_constraints(constraints))
                .collect(),
        ) {
            Some(CallableCandidateSelection::Selected(selected)) => declared(&selected.info)
                .then(|| ScopeConstructorSelection::Selected(Box::new(selected.info))),
            Some(CallableCandidateSelection::Ambiguous(candidates, _)) => {
                Some(ScopeConstructorSelection::Ambiguous(candidates))
            }
            Some(CallableCandidateSelection::MissingContext(_)) | None => candidates
                .iter()
                .all(declared)
                .then_some(ScopeConstructorSelection::Inapplicable),
        }
    }

    /// One selection over a bare call's receiver-less `functions` and the `constructors` that
    /// compete with them, each family on its own scope rung. kotlinc 2.4.21 calls a same-package or
    /// star-imported `fun Pair(a: Int, b: Int)` rather than the default-imported `kotlin.Pair`
    /// constructor, constructs the pair when that function does not apply, and lets an explicitly
    /// imported classifier outrank a same-package function. `None` when nothing competes or
    /// nothing applies, or when the functional-interface constructor wins: the call is then the
    /// constructor path's alone, as without a competing function.
    /// The shared selection over a bare call's `functions` and the classifier's constructors:
    /// `scope_constructors` when they are the call's whole family, `competing` when they compete
    /// with `functions`.
    pub(super) fn scope_call_decision(
        &self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        call: ScopeConstructorChoiceCall<'_>,
        (functions, scope_constructors, competing): (
            &[crate::libraries::FunctionInfo],
            &[crate::libraries::FunctionInfo],
            &[crate::libraries::FunctionInfo],
        ),
    ) -> ScopeCallDecision {
        match self.compete_scope_constructors(scope, call_args, call, (functions, competing)) {
            Some(ScopeCallCompetition::Constructor(selected)) => ScopeCallDecision {
                constructor: Some(ScopeConstructorSelection::Selected(selected)),
                function: None,
            },
            Some(ScopeCallCompetition::Callable(selection)) => ScopeCallDecision {
                constructor: None,
                function: Some(selection),
            },
            None => ScopeCallDecision {
                constructor: self.scope_constructor_choice(
                    scope,
                    call_args,
                    call,
                    scope_constructors,
                ),
                function: None,
            },
        }
    }

    fn compete_scope_constructors(
        &self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        call: ScopeConstructorChoiceCall<'_>,
        (functions, constructors): (
            &[crate::libraries::FunctionInfo],
            &[crate::libraries::FunctionInfo],
        ),
    ) -> Option<ScopeCallCompetition> {
        let classifier = constructors.first()?.constructor?.classifier;
        let shape = self.resolved_type_name(classifier)?;
        if shape.own_type_parameter_count != shape.type_params().len() {
            return None;
        }
        let constraints = self.scope_constructor_constraints(
            call_args.call,
            call,
            classifier,
            shape.type_params().len(),
        );
        let family = functions
            .iter()
            .filter(|candidate| candidate.kind == crate::libraries::FnKind::TopLevel)
            .cloned()
            .chain(
                constructors
                    .iter()
                    .cloned()
                    .map(|candidate| candidate.with_constructor_call_constraints(constraints)),
            )
            .collect::<Vec<_>>();
        match self.select_callable_candidate(
            scope,
            call_args,
            call.explicit_type_args,
            None,
            CallResultConstraint::direct(call.expected),
            family,
        )? {
            CallableCandidateSelection::Selected(selected)
                if selected.info.constructor.is_some() =>
            {
                (selected
                    .info
                    .constructor
                    .map(|constructor| constructor.origin)
                    == Some(crate::libraries::ConstructorOrigin::Declared))
                .then(|| ScopeCallCompetition::Constructor(Box::new(selected.info)))
            }
            selection => Some(ScopeCallCompetition::Callable(selection)),
        }
    }

    /// Record the constructor of `internal` that a bare call reaches. The provider constructor
    /// path materializes only the shared selection's winner. When the shared selection found
    /// none applicable, the provider path reports what it can, and whatever it leaves unreported
    /// is reported over the candidates as a function family's rejection is.
    pub(super) fn record_scope_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        (internal, classifier): (TypeName, &crate::libraries::LibraryType),
        call: ScopeConstructorCall<'_>,
    ) -> Result<LibraryConstructorSelection, LibraryConstructorFailure> {
        if let Some(ScopeConstructorSelection::Ambiguous(candidates)) = call.selection {
            let message =
                Self::ambiguous_candidates_message(internal.nested_segment_ref(), candidates);
            self.diags
                .error(self.call_callee_name_span(call_args.call), message);
            return Ok(LibraryConstructorSelection::Rejected(Ty::Error));
        }
        let checkpoint = self.diags.diags.len();
        let recorded = self.record_library_constructor(
            scope,
            call_args.call,
            internal,
            classifier,
            call_args.args,
            LibraryConstructorOptions {
                arg_names: call.arg_names,
                applied_classifier: call.alias_target,
                bound_outer: None,
                expected: call.expected,
                priority: call.priority,
                selected: call.selection.map_or(
                    ConstructorPreselection::Open,
                    ScopeConstructorSelection::preselection,
                ),
            },
        );
        if !matches!(
            call.selection,
            Some(ScopeConstructorSelection::Inapplicable)
        ) {
            return recorded;
        }
        // A declaration the arguments map onto answers an unreported rejection: its inferred
        // instance types the call, as kotlinc keeps an inapplicable candidate's result.
        let mapped = match recorded {
            Ok(LibraryConstructorSelection::NoMatch(_)) => false,
            Ok(LibraryConstructorSelection::Rejected(_)) => true,
            _ => return recorded,
        };
        if self.diags.diags.len() != checkpoint {
            return recorded;
        }
        let rejected = self.report_single_mapped_candidate_type_errors(
            scope,
            call_args,
            internal.nested_segment_ref(),
            CallConstraints {
                type_args: call.explicit_type_args,
                receiver: Ty::Error,
                expected: call.expected,
            },
            call.candidates,
        );
        match rejected {
            Some(result) => {
                self.diags.sort_source_order_from(checkpoint);
                let result = if mapped {
                    self.ctor_result_name(scope, call_args.call, internal, call.expected, None)
                } else {
                    result
                };
                Ok(LibraryConstructorSelection::Rejected(result))
            }
            None => Ok(LibraryConstructorSelection::NoMatch(None)),
        }
    }
}
