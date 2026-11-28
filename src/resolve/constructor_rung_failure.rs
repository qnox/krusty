//! The inapplicable nested-constructor rung of a qualified call `C.Name(args)`.
//!
//! kotlinc's tower does not stop at the constructors of the nested classifier `C.Name`: when none
//! applies, the call continues to the later rungs that name `Name` through `C` (associated
//! functions, members and extensions of `C`'s companion value, function-valued properties, static
//! callables). If no rung selects, kotlinc reports the failure group with the best applicability
//! across the whole tower. A lone group reports its own diagnostic, and two equally applicable
//! groups report "none of the following candidates is applicable:" over both of them.
//!
//! The constructor rung therefore records its failure here instead of reporting it. Each later
//! rung, when its own collection and selection reject the call, settles the recorded failure
//! against its own applicability before reporting.

use super::*;

/// How close a rung's rejected candidates came to accepting the call, ordered from worst to best
/// as kotlinc ranks a failed candidate group.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum RungApplicability {
    /// The rung has no candidate named by the call.
    NoCandidate,
    /// No candidate maps the arguments onto its parameters.
    ArgumentMapping,
    /// A candidate maps the arguments but rejects their types.
    TypeMismatch,
}

/// The constructor rung's own report, replayed when it owns the call's failure.
pub(super) enum ConstructorFailureReport {
    Source {
        arguments: CtorDelegationCall,
        candidates: Vec<CtorDelegationCandidate>,
        class_name: String,
    },
    Library(Option<LibraryConstructorFailure>),
}

pub(super) struct DeferredConstructorFailure {
    applicability: RungApplicability,
    report: ConstructorFailureReport,
}

impl DeferredConstructorFailure {
    pub(super) fn source(
        arguments: CtorDelegationCall,
        candidates: Vec<CtorDelegationCandidate>,
        class_name: String,
        trailing_lambda: bool,
    ) -> Self {
        let names: Option<&[Option<String>]> = arguments
            .names
            .iter()
            .any(Option::is_some)
            .then_some(&arguments.names);
        let applicability = if candidates.is_empty() {
            RungApplicability::NoCandidate
        } else if candidates.iter().any(|candidate| {
            let params = crate::libraries::ParamList {
                visibility: Visibility::Public,
                names: candidate.param_names.clone(),
                defaults: candidate.defaults.as_slice().into(),
                types: candidate.target.params().to_vec(),
                recv_fun: candidate.lambda_receivers.as_slice().into(),
                vararg: candidate.vararg,
                annotation: None,
            };
            map_param_list_args(&arguments.args, names, &params, trailing_lambda).is_ok()
        }) {
            RungApplicability::TypeMismatch
        } else {
            RungApplicability::ArgumentMapping
        };
        Self {
            applicability,
            report: ConstructorFailureReport::Source {
                arguments,
                candidates,
                class_name,
            },
        }
    }

    pub(super) fn library(
        failure: Option<LibraryConstructorFailure>,
        rejected: Option<&RejectedProviderConstructors>,
    ) -> Self {
        let applicability = match &failure {
            Some(LibraryConstructorFailure::Mapping(_)) => RungApplicability::ArgumentMapping,
            Some(_) => RungApplicability::TypeMismatch,
            None => match rejected {
                None => RungApplicability::NoCandidate,
                Some(rejected) if rejected.type_mismatches().next().is_some() => {
                    RungApplicability::TypeMismatch
                }
                Some(_) => RungApplicability::ArgumentMapping,
            },
        };
        Self {
            applicability,
            report: ConstructorFailureReport::Library(failure),
        }
    }
}

impl Checker<'_> {
    /// Keep the constructor rung's failure for the later rungs of `call`.
    pub(super) fn defer_constructor_failure(
        &mut self,
        call: ExprId,
        failure: DeferredConstructorFailure,
    ) {
        self.deferred_constructor_failures.insert(call, failure);
    }

    pub(super) fn has_deferred_constructor_failure(&self, call: ExprId) -> bool {
        self.deferred_constructor_failures.contains_key(&call)
    }

    /// Settle `call`'s deferred constructor failure against a later rung that rejects the call
    /// with `later` applicability. Returns whether the failure was reported here; otherwise the
    /// later rung reports its own, better, failure (or there was no deferred constructor rung).
    pub(super) fn settle_deferred_constructor_failure(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        later: RungApplicability,
    ) -> bool {
        let Some(deferred) = self.deferred_constructor_failures.remove(&call) else {
            return false;
        };
        match later.cmp(&deferred.applicability) {
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal if later != RungApplicability::NoCandidate => {
                self.diags.error(
                    self.call_callee_name_span(call),
                    INAPPLICABLE_OVERLOAD_PREFIX.to_string(),
                );
                true
            }
            _ => {
                self.report_constructor_failure(scope, call, args, deferred.report);
                true
            }
        }
    }

    /// The applicability of a rung whose function candidates all rejected `call`.
    pub(super) fn callable_rung_applicability(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        type_args: &[Ty],
        overloads: &[crate::libraries::FunctionInfo],
    ) -> RungApplicability {
        if overloads.is_empty() {
            return RungApplicability::NoCandidate;
        }
        match self.callable_mapping_failures(scope, call, args, type_args, overloads) {
            None => RungApplicability::TypeMismatch,
            Some(_) => RungApplicability::ArgumentMapping,
        }
    }

    fn report_constructor_failure(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        report: ConstructorFailureReport,
    ) {
        let span = self.span(call);
        match report {
            ConstructorFailureReport::Source {
                arguments,
                candidates,
                class_name,
            } => {
                if !self.report_source_constructor_mapping_error(
                    call,
                    args,
                    &arguments,
                    &candidates,
                    &class_name,
                ) && !self.call_already_has_argument_diagnostic(call, args)
                {
                    self.diags
                        .error(span, INAPPLICABLE_OVERLOAD_PREFIX.to_string());
                }
            }
            ConstructorFailureReport::Library(Some(failure)) => {
                self.report_library_constructor_failure(scope, call, args, failure);
            }
            ConstructorFailureReport::Library(None) => {
                if !self.call_already_has_argument_diagnostic(call, args) {
                    self.diags
                        .error(span, INAPPLICABLE_OVERLOAD_PREFIX.to_string());
                }
            }
        }
    }
}
