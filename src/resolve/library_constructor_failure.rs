//! Why a library or dependency classifier's constructor call failed, and its kotlinc report: an
//! argument-mapping failure, a rejected argument, or a type argument outside its bound.

use super::*;

pub(super) struct LibraryConstructorMappingFailure {
    failure: CallArgMappingFailure,
    params: Vec<Ty>,
    call_sig: CallSig,
    class_name: String,
}

pub(super) enum LibraryConstructorFailure {
    Mapping(Box<LibraryConstructorMappingFailure>),
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
            Self::Mapping(_) | Self::TypeArgumentMismatch { .. } => None,
        }
    }
}

impl Checker<'_> {
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
            LibraryConstructorFailure::TypeArgumentMismatch { expected, actual } => {
                self.expect_assignable(expected, actual, self.span(call), "type argument");
            }
        }
    }
}
