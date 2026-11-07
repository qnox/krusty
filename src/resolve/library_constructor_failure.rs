//! Why a library or dependency classifier's constructor call failed, and its kotlinc report: an
//! argument-mapping failure, a rejected argument, or a type argument outside its bound.

use super::*;

/// Receiver-specialized declaration evidence for an inner constructor's bound diagnostics.
pub(super) struct BoundConstructorDiagnostic {
    pub(super) outer: Ty,
    pub(super) signature: crate::libraries::GenericSig,
}

impl BoundConstructorDiagnostic {
    pub(super) fn new(outer: Ty, signature: crate::libraries::GenericSig) -> Box<Self> {
        Box::new(Self { outer, signature })
    }
}

pub(super) struct LibraryConstructorMappingFailure {
    failure: CallArgMappingFailure,
    params: Vec<Ty>,
    call_sig: CallSig,
    class_name: String,
}

/// A postponed function argument belongs to the rejected constructor's constraint system even
/// when substituting its fixed type arguments gives the same displayed function type. References
/// retain their selected declaration's diagnostic; lambdas retain their assignability diagnostic.
pub(super) struct RejectedConstructorFunctionArgument {
    argument: ExprId,
    expected: Ty,
    actual: Ty,
    diagnostic: RejectedFunctionArgumentDiagnostic,
}

enum RejectedFunctionArgumentDiagnostic {
    Lambda,
    Reference(callable_reference_publication::SelectedReferenceDiagnostic),
}

pub(super) enum LibraryConstructorFailure {
    Mapping(Box<LibraryConstructorMappingFailure>),
    /// The constructor's constraint system rejected these arguments.
    ArgumentContradictions(Box<super::generic_argument_bounds::ArgumentContradictions>),
    TypeMismatch {
        source_argument: usize,
        expected: Ty,
        actual: Ty,
        /// The invalid expression still has its explicit/specialized construction type. Retaining
        /// it after reporting the argument error lets the enclosing expectation diagnose an
        /// independent result-type contradiction; no constructor target is committed to FIR.
        result: Option<Ty>,
    },
    TypeArgumentMismatch {
        expected: Ty,
        actual: Ty,
        classifier: TypeName,
        /// The call's explicit type arguments, parallel to the classifier's formals.
        explicit: Vec<Ty>,
        /// kotlinc's rendering of the first constructor rejected by the bound.
        constructor: String,
        bound_constructor: Option<Box<BoundConstructorDiagnostic>>,
        postponed_arguments: Vec<RejectedConstructorFunctionArgument>,
    },
}

impl LibraryConstructorFailure {
    pub(super) fn mapping(
        failure: CallArgMappingFailure,
        params: Vec<Ty>,
        call_sig: CallSig,
        class_name: String,
    ) -> Self {
        Self::Mapping(Box::new(LibraryConstructorMappingFailure {
            failure,
            params,
            call_sig,
            class_name,
        }))
    }

    pub(super) fn result_type(&self) -> Option<Ty> {
        match self {
            Self::TypeMismatch { result, .. } => *result,
            Self::Mapping(_)
            | Self::ArgumentContradictions(_)
            | Self::TypeArgumentMismatch { .. } => None,
        }
    }
}

impl Checker<'_> {
    pub(super) fn rejected_constructor_function_arguments(
        &self,
        args: &[ExprId],
        argument_slots: &[usize],
        signature: &crate::libraries::GenericSig,
        explicit: &[Ty],
    ) -> Vec<RejectedConstructorFunctionArgument> {
        let bindings = crate::symbol_resolver::seeded_gsig_binds(signature, explicit);
        args.iter()
            .zip(argument_slots)
            .filter_map(|(&argument, &parameter)| {
                let (actual, diagnostic) = match self.file.expr(argument) {
                    Expr::Lambda { .. } => (
                        self.expr_types[argument.0 as usize],
                        RejectedFunctionArgumentDiagnostic::Lambda,
                    ),
                    Expr::CallableRef { .. } => (
                        *self.callable_reference_types.get(&argument)?,
                        RejectedFunctionArgumentDiagnostic::Reference(
                            self.selected_callable_reference_diagnostics
                                .get(&argument)?
                                .clone(),
                        ),
                    ),
                    _ => return None,
                };
                let declared = *signature.params.get(parameter)?;
                let expected = crate::types::ty_subst_keep_unbound(declared, &bindings);
                (expected.non_null().is_fun()
                    && actual.non_null().is_fun()
                    && !actual.mentions_error()
                    && !actual.mentions_pending())
                .then_some(RejectedConstructorFunctionArgument {
                    argument,
                    expected,
                    actual,
                    diagnostic,
                })
            })
            .collect()
    }

    /// The bound and argument of the first of a constructor's explicit type arguments `explicit`
    /// outside its bound, when every one of the classifier's `formals` is given explicitly.
    pub(super) fn explicit_constructor_bound_violation<F: crate::types::TyParamKey>(
        &self,
        formals: &[F],
        signature: &crate::libraries::GenericSig,
        explicit: &[Ty],
    ) -> Option<(Ty, Ty)> {
        let fully_explicit = explicit.len() == formals.len()
            && explicit
                .iter()
                .all(|argument| !argument.mentions_error() && !argument.mentions_pending());
        if !fully_explicit {
            return None;
        }
        let bindings = formals
            .iter()
            .map(|formal| crate::types::TyParamName::new(formal.borrow()))
            .zip(explicit.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        crate::symbol_resolver::generic_binding_bound_violation(
            signature,
            &bindings,
            |actual, bound| self.generic_bound_admits(actual, bound),
        )
    }

    /// Report `failure`, the reason no constructor of the called classifier accepted `call`.
    pub(super) fn report_library_constructor_failure(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        failure: LibraryConstructorFailure,
    ) {
        match failure {
            LibraryConstructorFailure::Mapping(mapping) => {
                let LibraryConstructorMappingFailure {
                    failure,
                    params,
                    call_sig,
                    class_name,
                } = *mapping;
                if !self.report_constructor_too_many_arguments(
                    call,
                    args,
                    &failure,
                    &class_name,
                    &params,
                    &call_sig.param_names,
                    &call_sig.param_defaults,
                    call_sig.vararg_index,
                ) {
                    self.report_call_arg_mapping_error(call, args, failure);
                }
            }
            LibraryConstructorFailure::ArgumentContradictions(contradictions) => {
                self.report_argument_contradictions(call, *contradictions);
            }
            LibraryConstructorFailure::TypeMismatch {
                source_argument,
                expected,
                actual,
                ..
            } => {
                let Some(&argument) = args.get(source_argument) else {
                    return;
                };
                // A context-sensitive lambda deliberately carries `Ty::Error` until a callable
                // candidate supplies a function/SAM shape. When the only mapped constructor
                // parameter is non-functional, materialize the lambda's natural source type solely
                // for the rejected-argument diagnostic; no constructor target or conversion is
                // committed. This reports the actual `() -> Unit` versus `String!` contradiction
                // instead of suppressing it as an ordinary propagated error sentinel.
                let actual = if actual == Ty::Error
                    && matches!(self.file.expr(argument), Expr::Lambda { .. })
                {
                    self.check_postponed_argument(scope, argument)
                } else {
                    actual
                };
                self.expect_assignable(expected, actual, self.span(argument), "argument");
            }
            LibraryConstructorFailure::TypeArgumentMismatch {
                expected,
                actual,
                classifier,
                explicit,
                constructor,
                bound_constructor,
                postponed_arguments,
            } => {
                let has_postponed_lambda = postponed_arguments.iter().any(|argument| {
                    matches!(
                        argument.diagnostic,
                        RejectedFunctionArgumentDiagnostic::Lambda
                    )
                });
                let reported_bound = bound_constructor.is_some_and(|bound| {
                    let display = format!("{}.{}", bound.outer.source_name(), constructor);
                    self.report_bound_inner_constructor_type_arguments(
                        call,
                        &explicit,
                        &bound,
                        &display,
                        !has_postponed_lambda,
                    )
                });
                if !reported_bound
                    && !self.report_explicit_constructor_type_argument_bounds(
                        call,
                        classifier,
                        !args.is_empty() && !has_postponed_lambda,
                        &explicit,
                        &constructor,
                    )
                {
                    self.expect_assignable(expected, actual, self.span(call), "type argument");
                }
                for constraint in postponed_arguments {
                    // The failed declaration bound is already proof that this postponed
                    // constraint cannot complete. An assignability probe would discard it
                    // merely because both substituted types happen to display alike.
                    match constraint.diagnostic {
                        RejectedFunctionArgumentDiagnostic::Lambda => {
                            self.report_assignability_error(
                                constraint.expected,
                                constraint.actual,
                                self.span(constraint.argument),
                                "argument",
                            );
                        }
                        RejectedFunctionArgumentDiagnostic::Reference(declaration) => {
                            self.diags
                                .error(declaration.span, declaration.into_message());
                        }
                    }
                }
                for &argument in args {
                    self.report_unbound_call_result_type_parameter(scope, argument);
                }
            }
        }
    }
}
