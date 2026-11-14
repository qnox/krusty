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
    Ambiguous,
    /// No constructor applies. The provider path reports why, but never commits; what it leaves
    /// unreported is reported over the candidates, as for a function family.
    Inapplicable,
}

impl ScopeConstructorSelection {
    pub(super) fn selected(&self) -> Option<&crate::libraries::FunctionInfo> {
        match self {
            Self::Selected(selected) => Some(selected),
            Self::Ambiguous | Self::Inapplicable => None,
        }
    }

    fn preselection(&self) -> ConstructorPreselection<'_> {
        match self {
            Self::Selected(selected) => ConstructorPreselection::Selected(selected),
            Self::Ambiguous => ConstructorPreselection::Ambiguous,
            Self::Inapplicable => ConstructorPreselection::Rejected,
        }
    }
}

/// What the shared selection over a bare call's scope constructors reads from the call.
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
        // A type alias application fixes the classifier arguments it spells; the ones it leaves
        // open are inferred like any call's, and the applied type constrains the result.
        let applied = self.alias_constructor_fixed_application(call_args.call, call.alias_target);
        let alias_type_args = applied
            .filter(|applied| {
                applied.kotlin_class_internal() == Some(classifier)
                    && applied.type_args().len() == shape.type_params().len()
            })
            .map(|applied| applied.type_args().to_vec());
        let type_args = alias_type_args
            .as_deref()
            .unwrap_or(call.explicit_type_args);
        let expected = Self::alias_constructor_result_constraint(applied, call.expected);
        let declared = |candidate: &crate::libraries::FunctionInfo| {
            candidate.constructor.map(|constructor| constructor.origin)
                == Some(crate::libraries::ConstructorOrigin::Declared)
        };
        match self.select_callable_candidate(
            scope,
            call_args,
            type_args,
            None,
            CallResultConstraint::direct(expected),
            candidates.to_vec(),
        ) {
            Some(CallableCandidateSelection::Selected(selected)) => declared(&selected.info)
                .then(|| ScopeConstructorSelection::Selected(Box::new(selected.info))),
            Some(CallableCandidateSelection::Ambiguous(..)) => {
                Some(ScopeConstructorSelection::Ambiguous)
            }
            Some(CallableCandidateSelection::MissingContext(_)) | None => candidates
                .iter()
                .all(declared)
                .then_some(ScopeConstructorSelection::Inapplicable),
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
