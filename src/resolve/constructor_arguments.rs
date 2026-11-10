//! Declaration-owned constructor argument shapes and contextual inference admission.

use super::*;

pub(super) enum SourceConstructorSelection {
    Selected {
        candidate_index: usize,
        resolved: Box<ResolvedCtorDelegation>,
    },
    NoMatch {
        mismatch: Option<ConstructorArgumentMismatch>,
        rejected: Vec<ConstructorCandidateRejection>,
    },
    Ambiguous,
}

#[derive(Clone, Copy)]
pub(super) struct ConstructorArgumentMismatch {
    pub(super) source_argument: usize,
    pub(super) expected: Ty,
    pub(super) actual: Ty,
}

/// A failed candidate's original first error, plus all contradictions from a complete mapping.
/// Incomplete argument shapes keep the legacy first error but publish no diagnostic peer evidence.
pub(super) struct ConstructorCandidateRejection {
    pub(super) candidate_index: usize,
    pub(super) first: Option<ConstructorArgumentMismatch>,
    pub(super) mapped_mismatches: Vec<ConstructorArgumentMismatch>,
    pub(super) argument_slots: Vec<usize>,
    pub(super) declared_arguments: Vec<Ty>,
}

impl ConstructorCandidateRejection {
    pub(super) fn new(candidate_index: usize) -> Self {
        Self {
            candidate_index,
            first: None,
            mapped_mismatches: Vec::new(),
            argument_slots: Vec::new(),
            declared_arguments: Vec::new(),
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
            self.report_assignability_error(
                mismatch.expected,
                mismatch.actual,
                self.span(argument),
                "argument",
            );
        }
        !reportable.is_empty()
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
        let slots = map_call_args(
            &arguments.args,
            Some(&arguments.names),
            &contextual.call_sig.param_names,
            contextual.params.len(),
            contextual.call_sig.required,
            &contextual.call_sig.param_defaults,
            visible_vararg,
            arguments.trailing_lambda,
        )
        .ok()?;
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
            let parameter = *params.get(slot)?;
            let expected = if candidate.vararg == Some(slot) {
                if self.file.is_spread_arg(argument)
                    || arguments.names.get(source).is_some_and(Option::is_some)
                {
                    parameter
                } else {
                    parameter.array_read_elem()?
                }
            } else {
                if self.file.is_spread_arg(argument) {
                    return None;
                }
                parameter
            };
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
            let nominal = self.expr_types[argument.0 as usize];
            let actual = self.argument_type_for_expected(scope, argument, nominal, expected);
            // Preserve the non-generic shell of a parameter while leaving only its type-variable
            // components to inference. A direct `T` cannot be judged against its erased non-null `Any`,
            // but `(Int) -> T` still requires a function of the right shape; otherwise that primary
            // constructor can enter competition for a String argument and make a valid String secondary
            // ambiguous.
            let constraint = candidate
                .parameter_constraints
                .get(slot)
                .copied()
                .unwrap_or_else(|| {
                    if !expected.mentions_ty_param() {
                        ConstructorParameterConstraint::Concrete
                    } else if expected.non_null().is_fun() {
                        ConstructorParameterConstraint::GenericFunction
                    } else {
                        ConstructorParameterConstraint::Inferred
                    }
                });
            let lambda_literal = matches!(self.file.expr(argument), Expr::Lambda { .. });
            let fits = self.sam_argument_fits(scope, expected, actual, argument)
                || match constraint {
                    // A lambda literal must first land in a FUNCTION slot; its actual type is contextual
                    // and is checked only after overload selection. Applying this rule here, in the shared
                    // constructor matcher, makes primary/secondary and file/module/classpath delegation
                    // agree instead of teaching the class-header path a private lambda exception.
                    ConstructorParameterConstraint::Concrete if lambda_literal => {
                        expected.non_null().is_fun()
                            || self.semantic_sam_signature(expected).is_some()
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
                            actual
                                .non_null()
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
                        lambda_literal
                            || self.generic_function_constructor_arg_fits(expected, actual)
                    }
                };
            // `Error` normally suppresses cascades, but a DEFERRED lambda deliberately uses it as a
            // placeholder. Its structural function-slot test is therefore authoritative and must
            // still reject a non-function constructor overload.
            if (lambda_literal || actual != Ty::Error) && !fits {
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
                let implicit_integer_coercion = self.implicit_integer_coercion_applies(
                    argument,
                    expected,
                    candidate
                        .implicit_integer_coercion
                        .get(slot)
                        .copied()
                        .unwrap_or(false),
                );
                if !implicit_integer_coercion {
                    let mismatch = ConstructorArgumentMismatch {
                        source_argument: source,
                        expected,
                        actual,
                    };
                    rejection.first.get_or_insert(mismatch);
                    mismatches.push(mismatch);
                }
            }
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
