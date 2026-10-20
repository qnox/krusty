//! Parameter-default checking in declaration order.
//!
//! Defaults share type checking and left-to-right parameter initialization across callable kinds.
//! Every parameter identity is in scope throughout the default list, but the current and later
//! parameters are uninitialized until their declaration point has been crossed.

use super::{Checker, CheckerScope, PlatformNarrowing};
use crate::ast::ExprId;
use crate::types::{Ty, TyKind};

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
        let has_receiver =
            matches!(expected.kind(), TyKind::Fun(function) if function.has_receiver);
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

    /// Check one callable's defaults in declaration order. All parameter identities shadow outer
    /// values from the beginning of the list, while reads or writes before a parameter's own
    /// declaration point report that exact parameter as uninitialized.
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
        let parameters = parameters.collect::<Vec<_>>();
        let identities = parameters
            .iter()
            .zip(parameter_types)
            .map(|(&(name, _), &ty)| {
                (name != "_").then(|| {
                    let identity = self.declare(scope, name, ty, false);
                    self.uninitialized_default_parameters
                        .insert(identity, name.to_string());
                    identity
                })
            })
            .collect::<Vec<_>>();
        for (((_name, default), &ty), identity) in
            parameters.into_iter().zip(parameter_types).zip(identities)
        {
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
            if let Some(identity) = identity {
                self.uninitialized_default_parameters.remove(&identity);
            }
        }
    }

    pub(super) fn report_uninitialized_default_parameter_read(
        &mut self,
        expression: ExprId,
        flow_identity: u32,
    ) {
        let Some(name) = self
            .uninitialized_default_parameters
            .get(&flow_identity)
            .cloned()
        else {
            return;
        };
        if self
            .reported_uninitialized_default_parameter_reads
            .insert(expression)
        {
            self.diags.error(
                self.span(expression),
                format!("parameter '{name}' is uninitialized here."),
            );
        }
    }

    pub(super) fn report_uninitialized_default_parameter_write(
        &mut self,
        statement: crate::ast::StmtId,
        flow_identity: u32,
    ) {
        let Some(name) = self
            .uninitialized_default_parameters
            .get(&flow_identity)
            .cloned()
        else {
            return;
        };
        if self
            .reported_uninitialized_default_parameter_writes
            .insert(statement)
        {
            self.diags.error(
                self.assignment_target_span(statement),
                format!("parameter '{name}' is uninitialized here."),
            );
        }
    }
}
