//! Parameter-default checking in declaration order.
//!
//! Defaults share type checking and left-to-right parameter publication across callable kinds,
//! while their definite-assignment rule depends on the declaration owner: kotlinc checks
//! constructor defaults and deliberately omits that diagnostic for ordinary function defaults.

use super::{Checker, CheckerScope, PlatformNarrowing};
use crate::ast::ExprId;
use crate::types::Ty;

#[derive(Clone, Copy)]
pub(super) enum ParameterDefaultFlow {
    Function,
    Constructor,
}

impl ParameterDefaultFlow {
    fn checks_definite_assignment(self) -> bool {
        matches!(self, Self::Constructor)
    }
}

impl Checker<'_> {
    fn expect_default_argument(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        expected: Ty,
    ) {
        let has_receiver = matches!(expected, Ty::Fun(function) if function.has_receiver);
        let actual = self.check_argument_expected(scope, expression, expected, has_receiver, None);
        // A default fills its parameter. A platform result committed to a parameter that rejects
        // null is guarded in the `$default` method, the same transition a call argument guards.
        if !expected.upper_bound_admits_null() {
            self.narrow_platform_value(expected, expression, PlatformNarrowing::Argument);
        }
        self.expect_assignable(
            expected,
            actual,
            self.value_diagnostic_span(expression, actual),
            "default argument",
        );
    }

    /// Check one callable's defaults in declaration order and publish each parameter only after its
    /// own default has been checked. A parameter is not in scope in its own default and later
    /// parameters must not leak backwards. The caller supplies the receiver context and owner kind;
    /// this operation owns the ordering and the complete body-check boundary.
    pub(super) fn check_parameter_defaults<'p>(
        &mut self,
        scope: &CheckerScope<'_>,
        parameters: impl ExactSizeIterator<Item = (&'p str, Option<ExprId>)>,
        parameter_types: &[Ty],
        flow: ParameterDefaultFlow,
    ) {
        assert_eq!(
            parameters.len(),
            parameter_types.len(),
            "checked parameter declarations and semantic types must stay aligned"
        );
        for ((name, default), &ty) in parameters.zip(parameter_types) {
            if let Some(default) = default {
                if self.signature_defaults_only {
                    self.signature_default_expression_depth += 1;
                }
                self.expect_default_argument(scope, default, ty);
                if flow.checks_definite_assignment() {
                    self.finish_executable_root(default);
                }
                if self.signature_defaults_only {
                    self.signature_default_expression_depth -= 1;
                }
            }
            if name != "_" {
                self.declare(scope, name, ty, false);
            }
        }
    }
}
