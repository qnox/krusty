//! Parameter-default checking in declaration order.
//!
//! Defaults share type checking and left-to-right parameter initialization across callable kinds.
//! Every parameter identity is in scope throughout the default list, but the current and later
//! parameters are uninitialized until their declaration point has been crossed.
//!
//! A parameter's annotations are the suppression scope of its whole declaration, as kotlinc's
//! `@Suppress` on any enclosing element: its type reference and its default are checked inside
//! their policies.

use super::{Checker, CheckerScope, PlatformNarrowing};
use crate::ast::{AnnotationRef, ExprId};
use crate::types::{Ty, TyKind};

#[derive(Clone, Copy)]
pub(super) enum ParameterDefaultFlow {
    Function,
    Constructor,
}

/// One declared parameter: its identity, its default, and the annotations written on it, whose
/// policies (`@Suppress`) hold for its whole declaration, type reference and default included.
#[derive(Clone, Copy)]
pub(super) struct DeclaredParameter<'p> {
    pub(super) name: &'p str,
    pub(super) default: Option<ExprId>,
    pub(super) declaration: crate::diag::Span,
    pub(super) annotations: &'p [AnnotationRef],
    pub(super) annotation_args: &'p [Vec<ExprId>],
}

impl<'p> From<&'p crate::ast::Param> for DeclaredParameter<'p> {
    fn from(parameter: &'p crate::ast::Param) -> Self {
        Self {
            name: &parameter.name,
            default: parameter.default,
            declaration: parameter.span,
            annotations: &parameter.annotations,
            annotation_args: &parameter.annotation_args,
        }
    }
}

impl<'p> From<&'p crate::ast::PropParam> for DeclaredParameter<'p> {
    fn from(parameter: &'p crate::ast::PropParam) -> Self {
        Self {
            name: &parameter.name,
            default: parameter.default,
            declaration: parameter.declaration_span,
            annotations: &parameter.annotations,
            annotation_args: &parameter.annotation_args,
        }
    }
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
        // An annotation array literal default takes its declared parameter's element type, as the
        // annotation slot it fills does; it has no type of its own from its elements.
        let actual = self
            .annotation_array_declaration_context(scope, expression, expected)
            .unwrap_or_else(|| {
                self.check_expected_operand(scope, expression, expected, has_receiver, None)
            });
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

    /// The declared value types of a local function's `parameters`, each type reference resolved
    /// in `types` under its parameter's own policies, whose annotations resolve in `annotations`.
    pub(super) fn local_parameter_types(
        &mut self,
        annotations: &CheckerScope<'_>,
        types: &CheckerScope<'_>,
        parameters: &[crate::ast::Param],
    ) -> Vec<Ty> {
        parameters
            .iter()
            .map(|parameter| {
                self.with_parameter_policies(annotations, parameter.into(), |checker| {
                    super::semantic_value_parameter_ty(
                        checker.type_ref_ty(types, &parameter.ty),
                        parameter.is_vararg,
                    )
                })
            })
            .collect()
    }

    /// Check one callable's defaults in declaration order. All parameter identities shadow outer
    /// values from the beginning of the list, while reads or writes before a parameter's own
    /// declaration point report that exact parameter as uninitialized.
    pub(super) fn check_parameter_defaults<'p>(
        &mut self,
        scope: &CheckerScope<'_>,
        parameters: impl ExactSizeIterator<Item = DeclaredParameter<'p>>,
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
            .map(
                |(
                    &DeclaredParameter {
                        name, declaration, ..
                    },
                    &ty,
                )| {
                    (name != "_").then(|| {
                        let identity = self.declare(scope, name, ty, false);
                        self.record_lexical_declaration(
                            scope,
                            name,
                            super::lexical_bindings::LexicalValueRole::Parameter(declaration),
                        );
                        self.uninitialized_default_parameters
                            .insert(identity, name.to_string());
                        identity
                    })
                },
            )
            .collect::<Vec<_>>();
        for ((parameter, &ty), identity) in
            parameters.into_iter().zip(parameter_types).zip(identities)
        {
            if let Some(default) = parameter.default {
                let policy_depth = self.push_declaration_policies(
                    scope,
                    parameter.annotations,
                    parameter.annotation_args,
                );
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
                self.active_lexical_policies.truncate(policy_depth);
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
