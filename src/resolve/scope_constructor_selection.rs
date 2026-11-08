//! Bare `Name(args)` constructors of a classifier reached on the file/import scope tower,
//! selected through the shared callable selector as ordinary candidates carrying a constructor
//! facet. The provider constructor path then materializes only the selected declaration.

use super::*;

#[cfg(test)]
mod tests;

/// The syntax of a bare `Name(args)` constructor call reached on the file/import scope tower.
pub(super) struct ScopeConstructorCall<'a> {
    pub(super) arg_names: Option<&'a [Option<String>]>,
    pub(super) explicit_type_args: &'a [Ty],
    /// The type alias application the call was written through.
    pub(super) alias_target: Option<Ty>,
    pub(super) expected: Option<Ty>,
    /// The classifier declares a `@LowPriorityInOverloadResolution` constructor, so this
    /// selection covers only its ordinary ones.
    pub(super) has_low_priority: bool,
}

/// The shared selector's decision over a classifier's scope-tower constructors.
enum ScopeConstructorSelection {
    /// One constructor applies; only it is materialized.
    Selected(Box<crate::libraries::FunctionInfo>),
    /// Several constructors are equally specific. The provider path reports the ambiguity, but
    /// never commits a constructor of its own choosing.
    Ambiguous,
    /// No constructor of this priority tier applies. The provider path reports why, but never
    /// commits; what it leaves unreported is reported over these candidates, as for a function.
    Inapplicable(Vec<crate::libraries::FunctionInfo>),
}

impl ScopeConstructorSelection {
    fn preselection(&self) -> ConstructorPreselection<'_> {
        match self {
            Self::Selected(selected) => ConstructorPreselection::Selected(selected),
            Self::Ambiguous => ConstructorPreselection::Ambiguous,
            Self::Inapplicable(_) => ConstructorPreselection::Rejected,
        }
    }
}

impl Checker<'_> {
    /// Select and record the constructor of `internal` that a bare call reaches. A classifier
    /// that captures neither an outer instance nor an enclosing declaration's type parameters
    /// has its constructors selected as ordinary callable candidates, including when the call
    /// is written through a type alias. The alias's fixed application supplies the classifier
    /// arguments it spells and constrains the result. The provider constructor path then
    /// materializes only the selected declaration, or reports why none applies. A captured
    /// type parameter is bound from the call's lexical scope, which only the provider path does.
    pub(super) fn record_scope_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        (internal, classifier): (TypeName, &crate::libraries::LibraryType),
        call: ScopeConstructorCall<'_>,
    ) -> Result<LibraryConstructorSelection, LibraryConstructorFailure> {
        let priority = if call.has_low_priority {
            ConstructorPriorityTier::Ordinary
        } else {
            ConstructorPriorityTier::All
        };
        // A type alias application fixes the classifier arguments it spells; the ones it leaves
        // open are inferred like any call's, and the applied type constrains the result.
        let applied = self.alias_constructor_fixed_application(call_args.call, call.alias_target);
        let alias_type_args = applied
            .filter(|applied| {
                applied.kotlin_class_internal() == Some(internal)
                    && applied.type_args().len() == classifier.type_params().len()
            })
            .map(|applied| applied.type_args().to_vec());
        let explicit_type_args = alias_type_args
            .as_deref()
            .unwrap_or(call.explicit_type_args);
        let expected = Self::alias_constructor_result_constraint(applied, call.expected);
        let shared = (classifier.outer_instance.is_none()
            && classifier.own_type_parameter_count == classifier.type_params().len())
        .then(|| {
            let candidates = self.scope_constructor_candidates(
                scope,
                call_args,
                (internal, classifier),
                (explicit_type_args, call.arg_names),
                (expected, priority),
            );
            match self.select_callable_candidate(
                scope,
                call_args,
                explicit_type_args,
                None,
                CallResultConstraint::direct(expected),
                candidates.clone(),
            ) {
                Some(CallableCandidateSelection::Selected(selected)) => {
                    ScopeConstructorSelection::Selected(Box::new(selected.info))
                }
                Some(CallableCandidateSelection::Ambiguous(..)) => {
                    ScopeConstructorSelection::Ambiguous
                }
                Some(CallableCandidateSelection::MissingContext(_)) | None => {
                    ScopeConstructorSelection::Inapplicable(candidates)
                }
            }
        });
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
                priority,
                selected: shared.as_ref().map_or(
                    ConstructorPreselection::Open,
                    ScopeConstructorSelection::preselection,
                ),
            },
        );
        let Some(ScopeConstructorSelection::Inapplicable(candidates)) = shared else {
            return recorded;
        };
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
        // The provider path found nothing to report: the candidates the shared selector rejected
        // are reported as a function family's are, the one the arguments map onto typing the call.
        let rejected = self.report_single_mapped_candidate_type_errors(
            scope,
            call_args.call,
            internal.nested_segment_ref(),
            call_args.args,
            call_args.arg_tys,
            CallConstraints {
                type_args: call.explicit_type_args,
                receiver: Ty::Error,
                expected: call.expected,
            },
            &candidates,
        );
        match rejected {
            Some(result) => {
                self.diags.sort_source_order_from(checkpoint);
                let result = match mapped {
                    true => {
                        self.ctor_result_name(scope, call_args.call, internal, call.expected, None)
                    }
                    false => result,
                };
                Ok(LibraryConstructorSelection::Rejected(result))
            }
            None => Ok(LibraryConstructorSelection::NoMatch(None)),
        }
    }

    /// `internal`'s constructors of one priority tier as ordinary callable candidates of the
    /// bare call, for the shared selector. Eager lambda analysis narrows them as it narrows a
    /// receiver-less function family.
    fn scope_constructor_candidates(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        (internal, classifier): (TypeName, &crate::libraries::LibraryType),
        (explicit_type_args, arg_names): (&[Ty], Option<&[Option<String>]>),
        (expected, priority): (Option<Ty>, ConstructorPriorityTier),
    ) -> Vec<crate::libraries::FunctionInfo> {
        let low_priority =
            crate::types::type_name("kotlin/internal/LowPriorityInOverloadResolution");
        let candidates = self
            .constructor_declarations(internal, classifier)
            .into_iter()
            .filter(|constructor| {
                let low = constructor.annotations.contains(&low_priority);
                match priority {
                    ConstructorPriorityTier::All => true,
                    ConstructorPriorityTier::Ordinary => !low,
                    ConstructorPriorityTier::Low => low,
                }
            })
            .map(|constructor| {
                crate::libraries::FunctionInfo::scope_constructor(internal, classifier, constructor)
            })
            .collect::<Vec<_>>();
        let CallArgs { call, args, .. } = call_args;
        if args
            .iter()
            .any(|&argument| self.file.is_callable_syntax(argument))
        {
            let partial = args
                .iter()
                .map(|&argument| {
                    (!self.file.is_callable_syntax(argument))
                        .then(|| self.expr_types[argument.0 as usize])
                })
                .collect::<Vec<_>>();
            self.eager_lambda_family(
                scope,
                eager_lambda_analysis::EagerLambdaCall {
                    shape: UntypedLambdaCall {
                        args,
                        partial: &partial,
                        arg_names,
                        trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
                        type_args: explicit_type_args,
                        expected,
                    },
                    label: call_implicit_lambda_label(self.file, call),
                },
                candidates,
            )
        } else {
            candidates
        }
    }
}
