//! Declaration-owned constructor argument shapes and contextual inference admission.

use super::*;

fn constructor_parameter_constraint(expected: Ty) -> ConstructorParameterConstraint {
    if !expected.mentions_ty_param() {
        ConstructorParameterConstraint::Concrete
    } else if expected.non_null().is_fun() {
        ConstructorParameterConstraint::GenericFunction
    } else {
        ConstructorParameterConstraint::Inferred
    }
}

pub(super) enum SourceConstructorSelection {
    Selected {
        candidate_index: usize,
        resolved: Box<ResolvedCtorDelegation>,
    },
    NoMatch {
        mismatch: Option<ConstructorArgumentMismatch>,
        rejected: Vec<ConstructorCandidateRejection>,
    },
    /// The candidates that survived specificity. Inapplicable overloads are not in this set.
    Ambiguous { tied: Vec<usize> },
}

struct CheckedConstructorArgument {
    expected: Ty,
    mismatch: Option<ConstructorArgumentMismatch>,
}

#[derive(Clone, Copy)]
pub(super) struct ConstructorArgumentMismatch {
    pub(super) source_argument: usize,
    pub(super) expected: Ty,
    pub(super) actual: Ty,
}

/// A failed candidate's original first error, plus all contradictions from a complete mapping.
/// Incomplete argument shapes keep the legacy first error but publish no diagnostic peer evidence.
#[derive(Clone)]
pub(super) struct ConstructorCandidateRejection {
    pub(super) candidate_index: usize,
    pub(super) first: Option<ConstructorArgumentMismatch>,
    pub(super) mapped_mismatches: Vec<ConstructorArgumentMismatch>,
    pub(super) argument_slots: Vec<usize>,
    pub(super) declared_arguments: Vec<Ty>,
    /// Why the arguments could not be assigned to this candidate. A call that mapped and then
    /// failed a parameter type keeps that mismatch in [`Self::mapped_mismatches`] instead.
    pub(super) mapping_failure: Option<CallArgMappingFailure>,
}

impl ConstructorCandidateRejection {
    pub(super) fn new(candidate_index: usize) -> Self {
        Self {
            candidate_index,
            first: None,
            mapped_mismatches: Vec::new(),
            argument_slots: Vec::new(),
            declared_arguments: Vec::new(),
            mapping_failure: None,
        }
    }
}

/// What `Name(args)` types its arguments against when the name denotes a constructible classifier.
///
/// Both parameter vectors are in SOURCE-ARGUMENT order — labels have already been mapped onto
/// declaration slots — so index `i` always describes the `i`th written argument.
pub(super) struct ConstructionExpectations {
    /// Per source argument: the parameter's semantic type and whether that type is a RECEIVER
    /// function type (`Scope.() -> Unit`, whose lambda binds `this`). `None` where same-arity
    /// constructors disagree, so the argument keeps its expectation-free shape.
    pub(super) params: Vec<Option<(Ty, bool)>>,
    /// The same parameters with the classifier's own type variables still symbolic, where the
    /// declaration provides them. This is the template call-site type arguments substitute into.
    pub(super) symbolic: Vec<Option<Ty>>,
    /// The constructed classifier's type parameters, in declaration order.
    pub(super) type_params: Vec<crate::types::TyParamName>,
    /// Declaration bounds parallel to `type_params`, used when an expected applied supertype
    /// supplies the classifier arguments before a contextual constructor argument is checked.
    pub(super) type_param_bounds: Vec<Vec<Ty>>,
    /// Declaration signatures of every constructor contributing to the mapped consensus.
    signatures: Vec<Option<crate::libraries::GenericSig>>,
}

impl Checker<'_> {
    /// Report why the only constructor a delegation or entry could reach rejected its arguments.
    ///
    /// kotlinc checks `super(...)`, `this(...)`, a class-header base call, and an enum entry's
    /// arguments like any call: a single candidate whose arguments all map reports each argument
    /// that does not fit its parameter, at that argument. Returns whether anything was reported,
    /// so the caller keeps its own diagnostic for a mapping failure or several candidates.
    pub(super) fn report_single_constructor_argument_mismatches(
        &mut self,
        scope: &CheckerScope<'_>,
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
    ) -> bool {
        let ([_], [rejection]) = (candidates, rejected) else {
            return false;
        };
        let reportable = rejection
            .mapped_mismatches
            .iter()
            .filter(|mismatch| mismatch.actual != Ty::Error && mismatch.expected != Ty::Error)
            .filter_map(|mismatch| {
                Some((*arguments.args.get(mismatch.source_argument)?, *mismatch))
            })
            .collect::<Vec<_>>();
        for &(argument, mismatch) in &reportable {
            // An argument call whose own formals the parameter leaves uninferred reports that
            // first, and renders them as uninferred in the mismatch (`super(arrayOf())`).
            if self.report_uninferred_call_argument(
                scope,
                argument,
                mismatch.expected,
                mismatch.actual,
            ) {
                continue;
            }
            self.report_assignability_error(
                mismatch.expected,
                mismatch.actual,
                self.span(argument),
                "argument",
            );
        }
        !reportable.is_empty()
    }

    /// A delegation no constructor accepted. The shared selector already mapped every candidate;
    /// this reports that mapping.
    pub(super) fn report_rejected_delegation(
        &mut self,
        scope: &CheckerScope<'_>,
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
        site: (DeclId, usize, Span),
    ) {
        let (class, secondary, span) = site;
        let (candidates, rejected) = Self::rejections_keeping_type_mismatches(candidates, rejected);
        let reported_types = self.report_single_constructor_argument_mismatches(
            scope,
            arguments,
            &candidates,
            &rejected,
        );
        let reported_excess =
            self.report_stored_excess(scope, class, arguments, &candidates, &rejected);
        if reported_types || reported_excess {
            return;
        }
        if self.report_type_mismatch_candidates(
            scope,
            class,
            secondary,
            span,
            &candidates,
            &rejected,
        ) {
            return;
        }
        if self.report_single_delegation_mapping(
            class,
            secondary,
            span,
            arguments,
            &candidates,
            &rejected,
        ) {
            return;
        }
        self.report_inapplicable_delegation_candidates(
            scope,
            class,
            secondary,
            span,
            &candidates,
            &rejected,
        );
    }

    /// Drop constructors that never mapped an argument once some other constructor did and rejected
    /// its type. kotlinc then reports that mismatch, or the list of such mismatches, and leaves the
    /// arity or name failure out. A constructor that mapped an argument stays, whether or not it is
    /// the delegating constructor.
    fn rejections_keeping_type_mismatches(
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
    ) -> (
        Vec<CtorDelegationCandidate>,
        Vec<ConstructorCandidateRejection>,
    ) {
        let reports_type = |rejection: &ConstructorCandidateRejection| {
            rejection
                .mapped_mismatches
                .iter()
                .any(|mismatch| mismatch.actual != Ty::Error && mismatch.expected != Ty::Error)
        };
        if !rejected.iter().any(reports_type) {
            return (candidates.to_vec(), rejected.to_vec());
        }
        let mut kept_index = Vec::new();
        let kept_candidates = candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                rejected
                    .iter()
                    .find(|rejection| rejection.candidate_index == *index)
                    .is_none_or(reports_type)
            })
            .map(|(index, candidate)| {
                kept_index.push(index);
                candidate.clone()
            })
            .collect();
        let kept_rejected = rejected
            .iter()
            .filter(|rejection| reports_type(rejection))
            .map(|rejection| {
                let mut rejection = rejection.clone();
                rejection.candidate_index = kept_index
                    .iter()
                    .position(|index| *index == rejection.candidate_index)
                    .unwrap_or(rejection.candidate_index);
                rejection
            })
            .collect();
        (kept_candidates, kept_rejected)
    }

    /// Several constructors each rejected a mapped argument. kotlinc lists those constructors and
    /// not the ones that failed before a type was checked.
    fn report_type_mismatch_candidates(
        &mut self,
        scope: &CheckerScope<'_>,
        class: DeclId,
        secondary: usize,
        span: Span,
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
    ) -> bool {
        let entries = rejected
            .iter()
            .filter(|rejection| {
                rejection
                    .mapped_mismatches
                    .iter()
                    .any(|mismatch| mismatch.actual != Ty::Error && mismatch.expected != Ty::Error)
            })
            .collect::<Vec<_>>();
        if entries.len() < 2 {
            return false;
        }
        let mut message = String::from("none of the following candidates is applicable:");
        for rejection in entries {
            let Some(candidate) = candidates.get(rejection.candidate_index) else {
                return false;
            };
            let Some(signature) = self.constructor_candidate_signature(scope, class, candidate)
            else {
                return false;
            };
            message.push_str(&format!("\n\n{signature}:"));
            for mismatch in &rejection.mapped_mismatches {
                if mismatch.actual == Ty::Error || mismatch.expected == Ty::Error {
                    continue;
                }
                message.push_str(&format!(
                    "\n  Argument type mismatch: actual type is '{}', but '{}' was expected.",
                    mismatch.actual.source_name(),
                    mismatch.expected.source_name()
                ));
            }
        }
        let span = self
            .delegation_keyword_span(class, secondary)
            .unwrap_or(span);
        self.diags.error(span, message);
        true
    }

    /// The one constructor a delegation could reach was given arguments past its parameter list.
    fn report_stored_excess(
        &mut self,
        scope: &CheckerScope<'_>,
        class: DeclId,
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
    ) -> bool {
        let ([candidate], [rejection]) = (candidates, rejected) else {
            return false;
        };
        let Some(failure) = rejection.mapping_failure.as_ref() else {
            return false;
        };
        if !failure
            .errors
            .iter()
            .all(|error| matches!(error, CallArgMappingError::TooManyArguments { .. }))
        {
            return false;
        }
        self.report_excess_arguments(scope, class, arguments, candidate, failure)
    }

    fn report_excess_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        class: DeclId,
        arguments: &CtorDelegationCall,
        candidate: &CtorDelegationCandidate,
        failure: &CallArgMappingFailure,
    ) -> bool {
        let Some(signature) = self.constructor_candidate_signature(scope, class, candidate) else {
            return false;
        };
        let message = format!("too many arguments for '{signature}'.");
        let mut reported = false;
        for error in &failure.errors {
            let CallArgMappingError::TooManyArguments { argument, .. } = error else {
                continue;
            };
            let Some(&expression) = arguments.args.get(*argument) else {
                continue;
            };
            self.diags.error(self.span(expression), message.clone());
            reported = true;
        }
        reported
    }

    /// One constructor rejected the call for a missing parameter or an unknown name.
    fn report_single_delegation_mapping(
        &mut self,
        class: DeclId,
        secondary: usize,
        span: Span,
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
    ) -> bool {
        let ([_candidate], [rejection]) = (candidates, rejected) else {
            return false;
        };
        let Some(failure) = rejection.mapping_failure.as_ref() else {
            return false;
        };
        let keyword = self
            .delegation_keyword_span(class, secondary)
            .unwrap_or(span);
        if failure
            .errors
            .iter()
            .all(|error| matches!(error, CallArgMappingError::MissingRequired { .. }))
        {
            for error in &failure.errors {
                self.diags.error(keyword, error.to_string());
            }
            return true;
        }
        let named = arguments.names.iter().any(Option::is_some);
        let only_names = failure.errors.iter().all(|error| {
            matches!(
                error,
                CallArgMappingError::NoParameterNamed { .. }
                    | CallArgMappingError::MissingRequired { .. }
            )
        });
        let unknown = failure
            .errors
            .iter()
            .any(|error| matches!(error, CallArgMappingError::NoParameterNamed { .. }));
        if !named || !only_names || !unknown {
            return false;
        }
        let mut missing = Vec::new();
        for error in &failure.errors {
            if let CallArgMappingError::MissingRequired { name } = error {
                missing.push(name.clone());
            }
        }
        for name in missing {
            self.diags
                .error(keyword, format!("no value passed for parameter '{name}'."));
        }
        for error in &failure.errors {
            let CallArgMappingError::NoParameterNamed { argument, .. } = error else {
                continue;
            };
            let name_span = arguments
                .name_spans
                .get(*argument)
                .copied()
                .flatten()
                .unwrap_or(keyword);
            self.diags.error(name_span, error.to_string());
        }
        true
    }

    /// Every candidate rejected the call for a missing parameter or an unknown name.
    fn report_inapplicable_delegation_candidates(
        &mut self,
        scope: &CheckerScope<'_>,
        class: DeclId,
        secondary: usize,
        span: Span,
        candidates: &[CtorDelegationCandidate],
        rejected: &[ConstructorCandidateRejection],
    ) -> bool {
        if candidates.len() < 2 || rejected.len() != candidates.len() {
            return false;
        }
        let mut entries = Vec::new();
        for rejection in rejected {
            let Some(candidate) = candidates.get(rejection.candidate_index) else {
                return false;
            };
            let Some(failure) = rejection.mapping_failure.as_ref() else {
                return false;
            };
            if failure.errors.iter().any(|error| {
                !matches!(
                    error,
                    CallArgMappingError::MissingRequired { .. }
                        | CallArgMappingError::NoParameterNamed { .. }
                )
            }) {
                return false;
            }
            let mut reasons = Vec::new();
            for error in &failure.errors {
                match error {
                    CallArgMappingError::MissingRequired { name } => {
                        reasons.push(format!("No value passed for parameter '{name}'."));
                    }
                    CallArgMappingError::NoParameterNamed { name, .. } => {
                        reasons.push(format!("No parameter with name '{name}' found."));
                    }
                    _ => return false,
                }
            }
            if reasons.is_empty() {
                return false;
            }
            let Some(signature) = self.constructor_candidate_signature(scope, class, candidate)
            else {
                return false;
            };
            entries.push((signature, reasons));
        }
        let mut message = String::from("none of the following candidates is applicable:");
        for (signature, reasons) in entries {
            message.push_str(&format!("\n\n{signature}:"));
            for reason in reasons {
                message.push_str(&format!("\n  {reason}"));
            }
        }
        let span = self
            .delegation_keyword_span(class, secondary)
            .unwrap_or(span);
        self.diags.error(span, message);
        true
    }

    /// A selected delegation that fills defaults the target cannot evaluate. A `this(...)` cycle is
    /// reported by the caller when the selected candidate is the delegating constructor.
    pub(super) fn report_selected_delegation_diagnostic(
        &mut self,
        span: Span,
        selected: &ResolvedCtorDelegation,
    ) -> bool {
        if !selected.omitted.is_empty() && selected.default_masks.is_empty() {
            self.diags.error(
                span,
                "constructor delegation cannot omit defaults for this target".to_string(),
            );
            return true;
        }
        false
    }

    /// The delegating constructor, when `delegating` is that secondary's index.
    pub(super) fn selected_constructor_is_delegating(
        candidates: &[CtorDelegationCandidate],
        candidate_index: usize,
        delegating: Option<usize>,
    ) -> bool {
        let Some(secondary) = delegating else {
            return false;
        };
        matches!(
            candidates
                .get(candidate_index)
                .map(|candidate| &candidate.target),
            Some(ResolvedCtorDelegationTarget::ThisSecondary { index, .. }) if *index == secondary
        )
    }

    /// A delegation chain that returns to a constructor. kotlinc reports that at every `this` in
    /// the chain. The delegating constructor's own span is the fallback when it wrote no keyword.
    pub(super) fn report_constructor_delegation_cycle(
        &mut self,
        declaration: &ClassDecl,
        cycle: &[usize],
    ) {
        for &index in cycle {
            let Some(constructor) = declaration.secondary_ctors.get(index) else {
                continue;
            };
            let span = if constructor.delegation_keyword.lo != 0 {
                constructor.delegation_keyword
            } else {
                constructor.span
            };
            self.diags
                .error(span, "there's a cycle in the delegation calls chain.");
        }
    }

    /// The written `this` or `super`. Delegation argument errors point there.
    pub(super) fn delegation_keyword_span(&self, class: DeclId, secondary: usize) -> Option<Span> {
        let Decl::Class(classifier) = self.file.decl(class) else {
            return None;
        };
        let keyword = classifier
            .secondary_ctors
            .get(secondary)?
            .delegation_keyword;
        (keyword.lo != 0).then_some(keyword)
    }

    /// Pick the constructor overload a delegation or construction call resolved to.
    ///
    /// The delegating constructor is an ordinary candidate. A `String` parameter forwarded to
    /// `Any` is a cycle: `String` is more specific, so the constructor accepts the call itself.
    /// An explicit `as Any` is what selects the erased parameter.
    pub(super) fn choose_scored_source_constructor(
        &self,
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        mut scored: Vec<super::source_constructors::ScoredSourceConstructor>,
        rejected: Vec<ConstructorCandidateRejection>,
    ) -> SourceConstructorSelection {
        if scored.iter().any(|candidate| !candidate.low_priority) {
            scored.retain(|candidate| !candidate.low_priority);
        }
        let constraint_shapes = scored
            .iter()
            .map(|candidate| Self::constructor_constraint_shape(candidates, candidate))
            .collect::<Vec<_>>();
        let undominated = constraint_shapes
            .iter()
            .enumerate()
            .map(|(index, shape)| {
                !constraint_shapes
                    .iter()
                    .enumerate()
                    .any(|(other_index, other)| {
                        index != other_index && Self::constraint_shape_dominates(other, shape)
                    })
            })
            .collect::<Vec<_>>();
        if undominated.iter().any(|keep| !keep) {
            scored = scored
                .into_iter()
                .zip(undominated)
                .filter_map(|(candidate, keep)| keep.then_some(candidate))
                .collect();
        }
        let Some(best) = scored.iter().map(|candidate| candidate.rank.0).max() else {
            let mismatch = (candidates.len() == 1)
                .then(|| {
                    rejected
                        .iter()
                        .find(|rejection| rejection.candidate_index == 0)?
                        .first
                })
                .flatten();
            return SourceConstructorSelection::NoMatch { mismatch, rejected };
        };
        scored.retain(|candidate| candidate.rank.0 == best);
        self.retain_without_equally_specific_tie_break_loss(&mut scored);
        let (candidate_index, resolved) = if let [candidate] = scored.as_slice() {
            (candidate.index, candidate.resolved.clone())
        } else {
            // Kotlin compares the parameters the supplied arguments map to, in argument order
            // (FIR's flat signature of the argument mapping), exactly as at a function call. A
            // parameter left to its default takes no part, a named argument compares against the
            // parameter it names, and each vararg element against the element type.
            let parameter_shapes = scored
                .iter()
                .map(|candidate| candidate.resolved.argument_types.clone())
                .collect::<Vec<_>>();
            let argument_kinds = arguments
                .args
                .iter()
                .map(|argument| {
                    super::integer_constants::call_arg_kind(
                        self.file,
                        *argument,
                        self.expr_types[argument.0 as usize],
                    )
                })
                .collect::<Vec<_>>();
            match crate::symbol_resolver::most_specific_parameter_shape_index(
                &self.fed_source(),
                &parameter_shapes,
                &argument_kinds,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected(index) => {
                    (scored[index].index, scored[index].resolved.clone())
                }
                crate::symbol_resolver::CandidateSelection::Ambiguous
                | crate::symbol_resolver::CandidateSelection::None => {
                    // Inference can make two applied parameter vectors identical even though one
                    // declaration keeps a stricter outer shape: `C<T>(List<T>)` is more specific
                    // than `C<T>(T)` for a `List<String>` argument. Compare the retained
                    // declaration constraints only after ordinary semantic specificity ties; they
                    // never make an inapplicable constructor applicable.
                    let constraint_shapes = scored
                        .iter()
                        .map(|candidate| Self::constructor_constraint_shape(candidates, candidate))
                        .collect::<Vec<_>>();
                    let mut unique = None;
                    for (index, shape) in constraint_shapes.iter().enumerate() {
                        let dominated =
                            constraint_shapes
                                .iter()
                                .enumerate()
                                .any(|(other_index, other)| {
                                    index != other_index
                                        && Self::constraint_shape_dominates(other, shape)
                                });
                        if !dominated && unique.replace(index).is_some() {
                            unique = None;
                            break;
                        }
                    }
                    if let Some(index) = unique {
                        (scored[index].index, scored[index].resolved.clone())
                    } else {
                        return SourceConstructorSelection::Ambiguous {
                            tied: scored.iter().map(|candidate| candidate.index).collect(),
                        };
                    }
                }
            }
        };
        SourceConstructorSelection::Selected {
            candidate_index,
            resolved: Box::new(resolved),
        }
    }

    /// Fewer omitted defaults, then a fixed arity over a vararg, wins only between equally
    /// specific constructors, as at a function call. Neither may discard a more specific
    /// constructor: `this(1)` beside `(x: Int, y: Int = 2)` and `(z: Long)` selects the `Int` one.
    fn retain_without_equally_specific_tie_break_loss(
        &self,
        scored: &mut Vec<super::source_constructors::ScoredSourceConstructor>,
    ) {
        if scored.len() < 2 {
            return;
        }
        let source = self.fed_source();
        let tie_break = |candidate: &super::source_constructors::ScoredSourceConstructor| {
            (candidate.rank.1, candidate.rank.2)
        };
        let dominated = scored
            .iter()
            .map(|candidate| {
                scored.iter().any(|other| {
                    tie_break(other) > tie_break(candidate)
                        && crate::symbol_resolver::declarations_equally_specific(
                            &source,
                            (None, &candidate.resolved.argument_types),
                            (None, &other.resolved.argument_types),
                        )
                })
            })
            .collect::<Vec<_>>();
        let mut dominated = dominated.into_iter();
        scored.retain(|_| !dominated.next().unwrap_or(false));
    }

    fn constructor_constraint_shape(
        candidates: &[CtorDelegationCandidate],
        candidate: &super::source_constructors::ScoredSourceConstructor,
    ) -> Vec<usize> {
        candidate
            .resolved
            .argument_slots
            .iter()
            .map(|slot| {
                candidates[candidate.index]
                    .parameter_constraints
                    .get(*slot)
                    .copied()
                    .map(Self::constraint_specificity)
                    .unwrap_or(0)
            })
            .collect()
    }

    fn constraint_specificity(constraint: ConstructorParameterConstraint) -> usize {
        match constraint {
            ConstructorParameterConstraint::Concrete => 2,
            ConstructorParameterConstraint::GenericConstructed
            | ConstructorParameterConstraint::GenericFunction => 1,
            ConstructorParameterConstraint::Inferred => 0,
        }
    }

    fn constraint_shape_dominates(other: &[usize], shape: &[usize]) -> bool {
        other.len() == shape.len()
            && other.iter().zip(shape).all(|(left, right)| left >= right)
            && other.iter().zip(shape).any(|(left, right)| left > right)
    }

    /// The parameter an already-placed argument is checked against, and the mismatch when it does
    /// not fit. `Error` suppresses a cascade, except a lambda literal whose placeholder is `Error`
    /// until overload selection gives it a function slot.
    fn checked_constructor_argument(
        &self,
        scope: &CheckerScope<'_>,
        arguments: &CtorDelegationCall,
        candidate: &CtorDelegationCandidate,
        source: usize,
        argument: ExprId,
        slot: usize,
    ) -> Option<CheckedConstructorArgument> {
        let parameter = candidate.target.params().get(slot).copied()?;
        let expected = if candidate.vararg == Some(slot)
            && !self.file.is_spread_arg(argument)
            && arguments
                .names
                .get(source)
                .is_none_or(|name| name.is_none())
        {
            parameter.array_read_elem()?
        } else if candidate.vararg != Some(slot) && self.file.is_spread_arg(argument) {
            return None;
        } else {
            parameter
        };
        let nominal = self.expr_types.get(argument.0 as usize).copied()?;
        let actual = self.argument_type_for_expected(scope, argument, nominal, expected);
        let constraint = candidate
            .parameter_constraints
            .get(slot)
            .copied()
            .unwrap_or_else(|| constructor_parameter_constraint(expected));
        let lambda_literal = matches!(self.file.expr(argument), Expr::Lambda { .. });
        let fits = self.constructor_argument_fits(scope, argument, expected, actual, constraint);
        let mismatch = if (lambda_literal || actual != Ty::Error) && !fits {
            let implicit_integer_coercion = self.implicit_integer_coercion_applies(
                argument,
                expected,
                candidate
                    .implicit_integer_coercion
                    .get(slot)
                    .copied()
                    .unwrap_or(false),
            );
            if implicit_integer_coercion {
                None
            } else {
                crate::trace_compiler!(
                    "expected_call",
                    "constructor argument rejected expression={argument:?} slot={slot} expected={expected:?} actual={actual:?} constraint={constraint:?} variances={:?}",
                    expected
                        .obj_internal()
                        .map(|owner| {
                            (0..expected.type_args().len())
                                .map(|index| {
                                    crate::assignable::TypeOracle::type_param_variance(
                                        self, owner, index,
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                );
                Some(ConstructorArgumentMismatch {
                    source_argument: source,
                    expected,
                    actual,
                })
            }
        } else {
            None
        };
        Some(CheckedConstructorArgument { expected, mismatch })
    }

    /// Whether one mapped constructor argument fits its parameter. The success path and an excess
    /// call that still mapped a prefix share this predicate.
    fn constructor_argument_fits(
        &self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        expected: Ty,
        actual: Ty,
        constraint: ConstructorParameterConstraint,
    ) -> bool {
        let lambda_literal = matches!(self.file.expr(argument), Expr::Lambda { .. });
        self.sam_argument_fits(scope, expected, actual, argument)
            || match constraint {
                // A lambda literal must first land in a FUNCTION slot; its actual type is contextual
                // and is checked only after overload selection. Applying this rule here, in the shared
                // constructor matcher, makes primary/secondary and file/module/classpath delegation
                // agree instead of teaching the class-header path a private lambda exception.
                ConstructorParameterConstraint::Concrete if lambda_literal => {
                    expected.non_null().is_fun() || self.semantic_sam_signature(expected).is_some()
                }
                // A numeric argument reaches another primitive slot the same way it does at a function
                // call (`L(b: Long)` accepts `L(1)`): the emit site inserts the conversion. Subtyping
                // alone rejected it, so every constructor with a `Long`/`Double`/… parameter was
                // unreachable from an integer literal — the one call origin that did not admit the
                // conversion `expect_assignable` applies everywhere else.
                //
                // `accepts_numeric` is the shared predicate and is deliberately the SAME one those
                // origins use, so this inherits their leniency rather than inventing a constructor
                // rule: it also admits `Int` into `Byte`/`Short`, where kotlinc requires an explicit
                // conversion. Narrowing that predicate is a project-wide decision (the plain-call and
                // classpath-constructor origins already take it), not one to fork here.
                ConstructorParameterConstraint::Concrete => {
                    self.receiver_is_assignable(actual, expected)
                        || expected.accepts_numeric(actual)
                        || self.argument_result_may_bind_expected(scope, argument, expected)
                }
                ConstructorParameterConstraint::Inferred => true,
                ConstructorParameterConstraint::GenericConstructed => {
                    if actual == Ty::Null {
                        expected.is_nullable()
                    } else if actual.is_nullable() && !expected.is_nullable() {
                        false
                    } else {
                        // A function type is an instance of its function classifier
                        // (`(A) -> R` is `Function1<A, R>`), so it reaches a constructed
                        // `Function1<F, T>` slot through that classifier.
                        crate::libraries::function_classifiers::supertype_classifier(
                            actual.non_null(),
                        )
                        .obj_internal()
                        .zip(expected.non_null().obj_internal())
                        .is_some_and(|(actual_owner, expected_owner)| {
                            self.generic_constructed_argument_reaches(
                                expected,
                                actual_owner,
                                expected_owner,
                            )
                        })
                    }
                }
                ConstructorParameterConstraint::GenericFunction => {
                    // A lambda's arity is contextual (an omitted parameter list can mean zero
                    // parameters or implicit `it`), so its provisional type cannot reject a function
                    // candidate. Non-lambda values already have a stable function shape and must match.
                    lambda_literal || self.generic_function_constructor_arg_fits(expected, actual)
                }
            }
    }

    pub(super) fn match_ctor_delegation_candidate(
        &self,
        scope: &CheckerScope<'_>,
        arguments: &CtorDelegationCall,
        candidate: &CtorDelegationCandidate,
        rejection: &mut ConstructorCandidateRejection,
    ) -> Option<ResolvedCtorDelegation> {
        let outer_receiver = match &candidate.target {
            ResolvedCtorDelegationTarget::Super {
                implicit_outer: Some(outer),
                ..
            } => {
                let expected = Ty::obj_name(*outer);
                let receiver = self
                    .implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| self.receiver_is_assignable(receiver.ty, expected))?;
                Some(self.implicit_receiver_selection(receiver))
            }
            ResolvedCtorDelegationTarget::ThisPrimary { .. }
            | ResolvedCtorDelegationTarget::ThisSecondary { .. }
            | ResolvedCtorDelegationTarget::Super {
                implicit_outer: None,
                ..
            } => None,
        };
        let params = candidate.target.params();
        let full_call_sig = CallSig::source(
            candidate.param_names.clone(),
            candidate.defaults.clone(),
            params
                .iter()
                .map(|parameter| match parameter.non_null().kind() {
                    TyKind::Fun(function) => function.params.clone(),
                    _ => Vec::new(),
                })
                .collect(),
            candidate.lambda_receivers.clone(),
            params
                .iter()
                .map(|parameter| match parameter.non_null().kind() {
                    TyKind::Fun(function) => function.context_count,
                    _ => 0,
                })
                .collect(),
            required_arity(params.len(), &candidate.defaults),
            candidate.vararg,
        );
        let contextual = self.contextual_call_shape(
            scope,
            params,
            &full_call_sig,
            candidate.context_count,
            arguments
                .names
                .iter()
                .any(Option::is_some)
                .then_some(arguments.names.as_slice()),
        )?;
        let visible_vararg = contextual.call_sig.vararg_index;
        let slots = match map_call_args(
            &arguments.args,
            Some(&arguments.names),
            crate::libraries::ParameterSlots {
                names: &contextual.call_sig.param_names,
                count: contextual.params.len(),
                required: contextual.call_sig.required,
                defaults: &contextual.call_sig.param_defaults,
                vararg: visible_vararg,
            },
            arguments.trailing_lambda,
        ) {
            Ok(slots) => slots,
            Err(failure) => {
                let placed_prefix_mismatches = failure
                    .errors
                    .iter()
                    .any(|error| matches!(error, CallArgMappingError::TooManyArguments { .. }))
                    && failure.errors.iter().all(|error| {
                        matches!(
                            error,
                            CallArgMappingError::TooManyArguments { .. }
                                | CallArgMappingError::MissingRequired { .. }
                        )
                    });
                if placed_prefix_mismatches {
                    for &(source, visible) in &failure.placed {
                        let Some(&slot) = contextual.parameter_indices.get(visible) else {
                            continue;
                        };
                        let Some(&argument) = arguments.args.get(source) else {
                            continue;
                        };
                        let Some(checked) = self.checked_constructor_argument(
                            scope, arguments, candidate, source, argument, slot,
                        ) else {
                            continue;
                        };
                        if let Some(mismatch) = checked.mismatch {
                            rejection.first.get_or_insert(mismatch);
                            rejection.mapped_mismatches.push(mismatch);
                        }
                    }
                }
                rejection.mapping_failure = Some(failure);
                return None;
            }
        };
        let argument_slots = arguments
            .args
            .iter()
            .map(|argument| {
                slots
                    .iter()
                    .position(|slot| slot.as_ref() == Some(argument))
                    .or(visible_vararg)
                    .and_then(|visible| contextual.parameter_indices.get(visible).copied())
            })
            .collect::<Option<Vec<_>>>()?;
        let mut mismatches = Vec::new();
        let mut argument_types = Vec::with_capacity(arguments.args.len());
        let mut argument_declared_types = Vec::with_capacity(arguments.args.len());
        let mut argument_implicit_integer_coercion = Vec::with_capacity(arguments.args.len());
        let mut argument_lambda_receivers = Vec::with_capacity(arguments.args.len());
        for (source, (&argument, &slot)) in arguments.args.iter().zip(&argument_slots).enumerate() {
            let checked = self.checked_constructor_argument(
                scope, arguments, candidate, source, argument, slot,
            )?;
            let expected = checked.expected;
            if let Some(mismatch) = checked.mismatch {
                rejection.first.get_or_insert(mismatch);
                mismatches.push(mismatch);
            }
            let declared_parameter = *candidate.target.declaration_params().get(slot)?;
            let declared = if candidate.vararg == Some(slot)
                && !self.file.is_spread_arg(argument)
                && arguments
                    .names
                    .get(source)
                    .is_none_or(|name| name.is_none())
            {
                declared_parameter.array_read_elem()?
            } else {
                declared_parameter
            };
            argument_types.push(expected);
            argument_declared_types.push(declared);
            argument_implicit_integer_coercion.push(
                candidate
                    .implicit_integer_coercion
                    .get(slot)
                    .copied()
                    .unwrap_or(false),
            );
            argument_lambda_receivers.push(
                candidate
                    .lambda_receivers
                    .get(slot)
                    .copied()
                    .unwrap_or_else(|| {
                        matches!(expected.non_null().kind(), TyKind::Fun(signature) if signature.has_receiver)
                    }),
            );
        }
        // Publish peer evidence only after every source argument has a valid mapped shape.
        // A later malformed spread/parameter must not turn a partial rejection into a typed peer.
        rejection.mapped_mismatches = mismatches;
        rejection.argument_slots = argument_slots.clone();
        rejection.declared_arguments = argument_declared_types.clone();
        if rejection.first.is_some() {
            return None;
        }
        let omitted = slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                // An omitted vararg with no declared default is an empty array, not a mask bit.
                // A declared default is a real omission: the constructor `$default` stub evaluates it.
                let implicit_empty_vararg =
                    visible_vararg == Some(index) && !contextual.call_sig.param_has_default(index);
                (slot.is_none() && !implicit_empty_vararg)
                    .then(|| contextual.parameter_indices.get(index).copied())
                    .flatten()
            })
            .collect::<Vec<_>>();
        let mut default_masks = Vec::new();
        if !omitted.is_empty() && candidate.supports_default_abi {
            default_masks.resize(params.len().div_ceil(32).max(1), 0);
            for &index in &omitted {
                default_masks[index / 32] |= (1u32 << (index % 32)) as i32;
            }
        }
        let capture_plan = match &candidate.target {
            ResolvedCtorDelegationTarget::Super { owner, .. } => {
                self.selected_constructor_capture_plan(scope, *owner)
            }
            _ => None,
        };
        Some(ResolvedCtorDelegation {
            target: candidate.target.clone(),
            capture_plan,
            outer_receiver,
            context_args: contextual.context_sources.into_iter().flatten().collect(),
            argument_slots,
            argument_types,
            argument_declared_types,
            argument_implicit_integer_coercion,
            argument_lambda_receivers,
            omitted,
            vararg: candidate.vararg,
            default_masks,
            parameter_constraints: candidate.parameter_constraints.clone(),
            low_priority: candidate.low_priority,
        })
    }

    /// The constructor shapes each argument of `Name(args)` is typed against, in SOURCE-ARGUMENT
    /// order. Providers normalize every declaration origin into the classifier's constructor list;
    /// ordinary callable mapping handles labels, defaults, varargs, and trailing lambdas once.
    ///
    /// The symbolic vector is the same mapped parameter list before the classifier's type variables
    /// are substituted. A slot is exposed only when every applicable constructor agrees, so this
    /// pre-selection expectation cannot choose an overload.
    pub(super) fn construction_argument_expectations(
        &self,
        internal: TypeName,
        arity: usize,
        arg_names: Option<&[Option<String>]>,
        trailing_lambda: bool,
    ) -> Option<ConstructionExpectations> {
        let classifier = self.resolved_type_name(internal)?;
        let declarations = self.constructor_declarations(internal, &classifier);
        let mut signatures = Vec::new();
        let candidates = declarations
            .iter()
            .filter_map(|constructor| {
                let slots = call_argument_parameter_indices(
                    arity,
                    constructor.params.len(),
                    arg_names,
                    trailing_lambda,
                    &constructor.call_sig,
                )?;
                let signature = crate::libraries::constructor_generic_signature(
                    internal,
                    &classifier,
                    constructor,
                );
                let symbolic = signature
                    .as_ref()
                    .filter(|generic| generic.params.len() == constructor.params.len());
                let parameters = slots
                    .into_iter()
                    .map(|slot| {
                        let parameter = (
                            constructor.params[slot],
                            constructor
                                .call_sig
                                .lambda_receiver_params
                                .get(slot)
                                .copied()
                                .unwrap_or(false),
                        );
                        let symbolic = symbolic.map(|generic| generic.params[slot]);
                        (parameter, symbolic)
                    })
                    .collect::<Vec<_>>();
                signatures.push(signature);
                Some(parameters)
            })
            .collect::<Vec<_>>();
        // A functional interface exposes a language-level single-argument constructor even though
        // its classifier has no JVM `<init>` declaration. Publish that synthetic declaration through
        // the same expectation table as ordinary constructors so lambdas and callable references are
        // contextually checked before either can emit diagnostics from an expectation-free probe.
        if candidates.is_empty()
            && arity == 1
            && arg_names.is_none_or(|names| names.iter().all(Option::is_none))
        {
            let sam = self.semantic_sam_signature(Ty::obj_name(internal))?;
            let function = Ty::fun_with_shape(
                sam.params,
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            return Some(ConstructionExpectations {
                params: vec![Some((function, false))],
                symbolic: vec![Some(function)],
                type_params: crate::types::TyParamName::all(&classifier.type_params),
                type_param_bounds: classifier.type_param_bounds().to_vec(),
                signatures: Vec::new(),
            });
        }
        let first = candidates.first()?;
        Some(ConstructionExpectations {
            params: first
                .iter()
                .enumerate()
                .map(|(source, &(expectation, _))| {
                    candidates
                        .iter()
                        .all(|candidate| candidate[source].0 == expectation)
                        .then_some(expectation)
                })
                .collect(),
            symbolic: first
                .iter()
                .enumerate()
                .map(|(source, &(_, symbolic))| {
                    candidates
                        .iter()
                        .all(|candidate| candidate[source].1 == symbolic)
                        .then_some(symbolic)
                        .flatten()
                })
                .collect(),
            type_params: crate::types::TyParamName::all(&classifier.type_params),
            type_param_bounds: classifier.type_param_bounds().to_vec(),
            signatures,
        })
    }

    /// Bind the constructed classifier's type parameters from everything known at the call BEFORE a
    /// contextual (lambda) argument is checked: explicit type arguments, the expected result, and
    /// the arguments already typed. `actuals` holds `Ty::Error` for a slot not yet typed.
    ///
    /// This is the constructor half of ordinary bidirectional inference. Lambda input shaping and
    /// admitted nested result constraints use it: `Store<ROOT, DOMAIN>(wrapper: Wrapper<ROOT>, toDomain: (ROOT) -> DOMAIN)`
    /// must hand `{ dto -> … }` a `dto: RootDto`, not the unbound `ROOT` (which erases to its bound
    /// and fails every member read in the body).
    pub(super) fn construction_type_param_binds(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        expected: Option<Ty>,
        plan: &ConstructionExpectations,
        actuals: &[Ty],
    ) -> crate::symbol_resolver::GSigBinds {
        let mut binds = crate::symbol_resolver::GSigBinds::default();
        if plan.type_params.is_empty() {
            return binds;
        }
        let fixed = self.constructor_fixed_bindings(scope, call, internal, expected, plan);
        for (slot, shape) in plan.symbolic.iter().enumerate() {
            let (Some(shape), Some(&actual)) = (*shape, actuals.get(slot)) else {
                continue;
            };
            if actual == Ty::Error || !shape.mentions_ty_param() {
                continue;
            }
            crate::symbol_resolver::unify_inferred_ty(shape, actual, &mut binds);
        }
        // Written and expected applications are fixed independently of argument inference.
        binds.extend(fixed);
        binds.retain(|name, ty| {
            plan.type_params.iter().any(|formal| formal == name) && *ty != Ty::Error
        });
        binds
    }

    fn constructor_fixed_bindings(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        expected: Option<Ty>,
        plan: &ConstructionExpectations,
    ) -> crate::symbol_resolver::GSigBinds {
        let mut expected_bindings = crate::symbol_resolver::GSigBinds::default();
        {
            let source = self.fed_source();
            crate::symbol_resolver::seed_unbound_constructor_result_from_symbols(
                &source,
                internal,
                &plan.type_params,
                &plan.type_param_bounds,
                expected,
                &mut expected_bindings,
            );
        }
        let mut explicit = crate::symbol_resolver::GSigBinds::default();
        if let Some(refs) = self
            .file
            .call_type_args
            .get(&call.0)
            .map(|syntax| syntax.arguments.clone())
        {
            for (name, type_ref) in plan.type_params.iter().zip(refs.iter()) {
                let ty = self.type_ref_ty_silent(scope, type_ref);
                if ty != Ty::Error {
                    explicit.insert(crate::types::TyParamName::new(name), ty);
                }
            }
        }
        expected_bindings.extend(explicit);
        expected_bindings
    }

    /// A rejected class application cannot supply result constraints to a nested producer.
    /// This does not retract declaration input types, including explicit lambda parameter context.
    pub(super) fn constructor_result_context_admitted(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        expected: Option<Ty>,
        plan: &ConstructionExpectations,
    ) -> bool {
        if plan.signatures.is_empty() || plan.type_params.is_empty() {
            return true;
        }
        let fixed = self.constructor_fixed_bindings(scope, call, internal, expected, plan);
        let Some(arguments) = plan
            .type_params
            .iter()
            .map(|formal| fixed.get(formal.as_str()).copied())
            .collect::<Option<Vec<_>>>()
        else {
            // An open application still needs ordinary argument inference and selection.
            return true;
        };
        !plan.signatures.iter().all(|signature| {
            signature.as_ref().is_some_and(|signature| {
                self.explicit_constructor_bound_violation(&plan.type_params, signature, &arguments)
                    .is_some()
            })
        })
    }

    /// Context shared by the qualified source and provider constructor selectors. Only a fixed
    /// declaration parameter agreed by the whole constructor family supplies literal context.
    pub(super) fn contextualize_constructor_arrays(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        arguments: &[ExprId],
        names: Option<&[Option<String>]>,
    ) -> Vec<(usize, Ty)> {
        let Some(expectations) = self.construction_argument_expectations(
            internal,
            arguments.len(),
            names,
            self.file.call_has_trailing_lambda.contains(&call.0),
        ) else {
            return Vec::new();
        };
        arguments
            .iter()
            .enumerate()
            .filter_map(|(index, &argument)| {
                let declared = expectations.symbolic[index]
                    .or_else(|| expectations.params[index].map(|(ty, _)| ty))?;
                let actual =
                    self.annotation_array_declaration_context(scope, argument, declared)?;
                Some((index, actual))
            })
            .collect()
    }

    /// A named argument mapped to the selected constructor's vararg parameter passes the whole
    /// array (`Base(parts = values)`), exactly as in an ordinary call. Constructor delegation and
    /// enum entries commit that mapping here; checked FIR would otherwise pack it as one element.
    pub(super) fn record_whole_array_delegation_arguments(
        &mut self,
        arguments: &CtorDelegationCall,
        selected: &ResolvedCtorDelegation,
    ) {
        let Some(vararg) = selected.vararg else {
            return;
        };
        for ((&argument, &slot), name) in arguments
            .args
            .iter()
            .zip(&selected.argument_slots)
            .zip(&arguments.names)
        {
            if slot == vararg && name.is_some() && !self.file.is_spread_arg(argument) {
                self.resolved_whole_array_vararg_args.insert(argument);
            }
        }
    }
}

impl Checker<'_> {
    /// Types the arguments of a `this(...)`/`super(...)` delegation ahead of constructor selection.
    ///
    /// A lambda or callable reference is contextual: its initial `Error` placeholder stays until the
    /// shared constructor selector maps it to a parameter slot, just like a primary class-header
    /// base argument.
    pub(super) fn type_delegation_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        arguments: &[ExprId],
    ) {
        for &argument in arguments {
            if !matches!(
                self.file.expr(argument),
                Expr::Lambda { .. } | Expr::CallableRef { .. }
            ) {
                self.expr(scope, argument);
            }
        }
    }
}
