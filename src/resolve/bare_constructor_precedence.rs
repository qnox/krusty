//! A bare `Name(args)` whose file/import tower holds both a receiver-less function and a
//! classifier. The classifier's constructors join that function level as ordinary candidates on
//! the classifier's own rung, and one shared selection decides which family takes the call.

use super::*;

/// Facts of one bare call that may rank a function family against a classifier's constructors.
pub(super) struct BareConstructorCompetition<'a> {
    pub(super) name: &'a str,
    pub(super) classifier: TypeName,
    pub(super) functions: &'a [crate::libraries::FunctionInfo],
    /// The type alias the call was written through, when the name resolved to one.
    pub(super) alias_target: Option<Ty>,
    pub(super) explicit_type_args: &'a [Ty],
    pub(super) expected: Option<Ty>,
    pub(super) arg_names: Option<&'a [Option<String>]>,
}

/// What a competing bare call leaves for the rest of `check_call`.
pub(super) struct SettledBareCall {
    /// The constructor-only selector still owns this name.
    pub(super) run_existing: bool,
    /// A function won, or the winning rung is ambiguous. `None` when the shared selection found
    /// nothing for the function path to finish.
    pub(super) top_level: Option<CallableCandidateSelection>,
    /// The combined family, when nothing in it applies.
    pub(super) inapplicable: Option<Vec<crate::libraries::FunctionInfo>>,
    /// A low-priority constructor deferred until closer receiver candidates have been tried.
    pub(super) low_priority: Option<(
        TypeName,
        crate::libraries::LibraryType,
        crate::libraries::FunctionInfo,
    )>,
    pub(super) inapplicable_constructor: bool,
    /// The provider constructors the selection rejected, kept for the call's diagnostics.
    pub(super) provider_rejections: Option<std::sync::Arc<RejectedProviderConstructors>>,
    pub(super) mapping_failure: Option<LibraryConstructorFailure>,
}

/// The one selection over a competing function family and the classifier's constructors.
pub(super) enum BareCallCompetition {
    /// No receiver-less function competes, or the constructor still belongs to its own selector
    /// (an alias application, an outer instance, an inherited nested classifier).
    Uncontested,
    /// An ordinary constructor on the winning rung. Materialize only this declaration.
    Constructor(crate::libraries::FunctionInfo),
    /// The winning constructor is `@LowPriorityInOverloadResolution`. It stays deferred until
    /// closer receiver-tower candidates have had their turn, and only this declaration is
    /// materialized then.
    LowPriorityConstructor(crate::libraries::FunctionInfo),
    /// A function won, or the winning rung is ambiguous. The function path consumes this decision
    /// and does not select again.
    Callable(CallableCandidateSelection),
    /// Nothing in the combined family applies. Report these candidates; do not select again.
    Inapplicable(Vec<crate::libraries::FunctionInfo>),
}

impl Checker<'_> {
    /// Rank `competition.functions` against the constructors of `competition.classifier`.
    ///
    /// kotlinc 2.4.21 calls a same-package or star-imported `fun Pair(a: Int, b: Int)` rather than
    /// the default-imported `kotlin.Pair` constructor, still constructs the pair when that function
    /// does not apply, and lets an explicitly imported classifier outrank a same-package function.
    /// An ambiguous nearer function family is reported as that ambiguity.
    pub(super) fn compete_bare_constructor_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        competition: BareConstructorCompetition<'_>,
    ) -> BareCallCompetition {
        if competition.alias_target.is_some() {
            return BareCallCompetition::Uncontested;
        }
        let (inherited, _) = self.inherited_nested_type_with_owner(competition.name);
        if inherited.found() == Some(competition.classifier) {
            return BareCallCompetition::Uncontested;
        }
        let Some(position) = self
            .resolver()
            .nearest_receiverless_function_level(competition.name)
        else {
            return BareCallCompetition::Uncontested;
        };
        let mut functions = competition
            .functions
            .iter()
            .filter(|candidate| candidate.kind == crate::libraries::FnKind::TopLevel)
            .cloned()
            .collect::<Vec<_>>();
        if functions.is_empty() {
            return BareCallCompetition::Uncontested;
        }
        for function in &mut functions {
            function.scope_rung = position.scope_rung;
            function.scope_rank = position.scope_rank;
        }
        let crate::symbol_resolver::CandidateSelection::Selected(selected) =
            self.resolver().scoped_classifier_in_scope(competition.name)
        else {
            return BareCallCompetition::Uncontested;
        };
        if selected.classifier != competition.classifier {
            return BareCallCompetition::Uncontested;
        }
        let Some(classifier) = self.resolved_type_name(competition.classifier) else {
            return BareCallCompetition::Uncontested;
        };
        if classifier.outer_instance.is_some() {
            return BareCallCompetition::Uncontested;
        }
        let mut constructors = self
            .constructor_declarations(competition.classifier, &classifier)
            .into_iter()
            .map(|constructor| {
                let mut candidate = crate::libraries::FunctionInfo::scope_constructor(
                    competition.classifier,
                    &classifier,
                    constructor,
                );
                candidate.scope_rung = selected.scope_rung;
                candidate.scope_rank = selected.scope_rank;
                candidate
            })
            .collect::<Vec<_>>();
        if constructors.is_empty() {
            return BareCallCompetition::Uncontested;
        }
        let mut family = functions;
        family.append(&mut constructors);
        let low_priority =
            crate::types::type_name("kotlin/internal/LowPriorityInOverloadResolution");
        match self.select_callable_candidate(
            scope,
            call_args,
            competition.explicit_type_args,
            None,
            CallResultConstraint::direct(competition.expected),
            family.clone(),
        ) {
            Some(CallableCandidateSelection::Selected(selected))
                if selected.info.constructor.is_some() =>
            {
                if selected.info.callable.annotations.contains(&low_priority) {
                    BareCallCompetition::LowPriorityConstructor(selected.info)
                } else {
                    BareCallCompetition::Constructor(selected.info)
                }
            }
            Some(selection) => BareCallCompetition::Callable(selection),
            None => BareCallCompetition::Inapplicable(family),
        }
    }

    /// Materialize a constructor the shared selection already chose, or leave the function
    /// decision for the rest of the call. `Break` is the call's type and ends checking.
    pub(super) fn settle_competing_bare_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        competition: BareConstructorCompetition<'_>,
    ) -> std::ops::ControlFlow<Ty, SettledBareCall> {
        let alias_target = competition.alias_target;
        let expected = competition.expected;
        let arg_names = competition.arg_names;
        let mut settled = SettledBareCall {
            run_existing: false,
            top_level: None,
            inapplicable: None,
            low_priority: None,
            inapplicable_constructor: false,
            provider_rejections: None,
            mapping_failure: None,
        };
        match self.compete_bare_constructor_call(scope, call_args, competition) {
            BareCallCompetition::Uncontested => settled.run_existing = true,
            BareCallCompetition::Callable(selection) => settled.top_level = Some(selection),
            BareCallCompetition::Inapplicable(family) => {
                settled.inapplicable_constructor = true;
                settled.inapplicable = Some(family);
            }
            BareCallCompetition::LowPriorityConstructor(selected) => {
                if let Some(internal) = selected
                    .constructor
                    .map(|constructor| constructor.classifier)
                {
                    if let Some(classifier) = self.resolved_type_name(internal) {
                        settled.low_priority = Some((internal, (*classifier).clone(), selected));
                    }
                }
            }
            BareCallCompetition::Constructor(selected) => {
                let Some(internal) = selected
                    .constructor
                    .map(|constructor| constructor.classifier)
                else {
                    return std::ops::ControlFlow::Continue(settled);
                };
                let Some(classifier) = self.resolved_type_name(internal) else {
                    return std::ops::ControlFlow::Continue(settled);
                };
                match self.record_library_constructor(
                    scope,
                    call_args.call,
                    internal,
                    &classifier,
                    call_args.args,
                    LibraryConstructorOptions {
                        arg_names,
                        applied_classifier: alias_target,
                        bound_outer: None,
                        expected,
                        priority: ConstructorPriorityTier::All,
                        selected: Some(&selected),
                    },
                ) {
                    Ok(LibraryConstructorSelection::Selected) => {
                        return std::ops::ControlFlow::Break(self.ctor_result_name(
                            scope,
                            call_args.call,
                            internal,
                            expected,
                            alias_target,
                        ));
                    }
                    Ok(LibraryConstructorSelection::Rejected) => {
                        return std::ops::ControlFlow::Break(Ty::Error);
                    }
                    // An invisible constructor does not take the call. The existing constructor
                    // path records it and lets every later tower level, including a receiver-less
                    // function that lost only to this constructor, compete.
                    Ok(LibraryConstructorSelection::Invisible(_)) => {
                        settled.run_existing = true;
                    }
                    Ok(LibraryConstructorSelection::NoMatch(rejections)) => {
                        settled.inapplicable_constructor = true;
                        settled.provider_rejections = rejections;
                    }
                    Err(error) => settled.mapping_failure = Some(error),
                }
            }
        }
        std::ops::ControlFlow::Continue(settled)
    }
}
