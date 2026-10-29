//! Bare `Name(args)` constructors of a classifier reached on the file/import scope tower,
//! selected through the shared callable selector as ordinary candidates carrying a constructor
//! facet. The provider constructor path then materializes only the selected declaration.

use super::*;

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
    /// No constructor of this priority tier applies. The provider path reports why, and commits
    /// only a sole declaration, the inapplicable candidate kotlinc reports against.
    Inapplicable,
}

impl ScopeConstructorSelection {
    fn preselection(&self) -> ConstructorPreselection<'_> {
        match self {
            Self::Selected(selected) => ConstructorPreselection::Selected(selected),
            Self::Ambiguous => ConstructorPreselection::Ambiguous,
            Self::Inapplicable => ConstructorPreselection::Rejected,
        }
    }
}

impl Checker<'_> {
    /// Select and record the constructor of `internal` that a bare call reaches. A classifier
    /// that captures no outer instance, written by its own name, has its constructors selected
    /// as ordinary callable candidates; the provider constructor path then materializes only the
    /// selected declaration, or reports why none applies.
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
        let shared =
            (classifier.outer_instance.is_none() && call.alias_target.is_none()).then(|| {
                self.select_scope_constructor(
                    scope,
                    call_args,
                    (internal, classifier),
                    (call.explicit_type_args, call.arg_names),
                    (call.expected, priority),
                )
            });
        self.record_library_constructor(
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
        )
    }

    /// Select among `internal`'s constructors of one priority tier as ordinary callable
    /// candidates of the bare call. Eager lambda analysis narrows them as it narrows a
    /// receiver-less function family.
    fn select_scope_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        (internal, classifier): (TypeName, &crate::libraries::LibraryType),
        (explicit_type_args, arg_names): (&[Ty], Option<&[Option<String>]>),
        (expected, priority): (Option<Ty>, ConstructorPriorityTier),
    ) -> ScopeConstructorSelection {
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
        let candidates = if args
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
        };
        match self.select_callable_candidate(
            scope,
            call_args,
            explicit_type_args,
            None,
            CallResultConstraint::direct(expected),
            candidates,
        ) {
            Some(CallableCandidateSelection::Selected(selected)) => {
                ScopeConstructorSelection::Selected(Box::new(selected.info))
            }
            Some(CallableCandidateSelection::Ambiguous(_)) => ScopeConstructorSelection::Ambiguous,
            Some(CallableCandidateSelection::MissingContext(_)) | None => {
                ScopeConstructorSelection::Inapplicable
            }
        }
    }
}
