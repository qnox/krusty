//! Exact selection and checking of signature-owned parameter defaults.
//!
//! Default expressions are Pass-1 executable payload, but selecting one must never select the
//! callable's ordinary body. This boundary keeps that stable-declaration selection separate from
//! bounded Pass-2 body selection while sharing the resolver's normal lexical scopes and expression
//! semantics.

use super::*;

impl Checker<'_> {
    fn expect_default_argument(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        expected: Ty,
    ) {
        let has_receiver = matches!(expected, Ty::Fun(function) if function.has_receiver);
        let actual = self.check_argument_expected(scope, expression, expected, has_receiver, None);
        self.expect_assignable(
            expected,
            actual,
            self.value_diagnostic_span(expression, actual),
            "default argument",
        );
    }

    /// Check one callable's selected defaults in declaration order and publish each parameter only
    /// after its own default has been checked. Kotlin permits a default to read preceding
    /// parameters, but a parameter is not in scope in its own default and later parameters must not
    /// leak backwards. Callers choose the receiver context; this operation owns the ordering rule.
    pub(super) fn check_parameter_defaults<'p>(
        &mut self,
        scope: &CheckerScope<'_>,
        parameters: impl ExactSizeIterator<Item = (&'p str, Option<ExprId>)>,
        parameter_types: &[Ty],
    ) {
        assert_eq!(
            parameters.len(),
            parameter_types.len(),
            "checked parameter declarations and semantic types must stay aligned"
        );
        for ((name, default), &ty) in parameters.zip(parameter_types) {
            if let Some(default) = default {
                let selected_default = self.selected_signature_default_declarations.is_some()
                    && self
                        .pending_signature_default_expressions
                        .contains(&default);
                if selected_default {
                    self.signature_default_expression_depth += 1;
                    self.expect_default_argument(scope, default, ty);
                    self.signature_default_expression_depth -= 1;
                    self.pending_signature_default_expressions.remove(&default);
                } else if self.selected_signature_default_declarations.is_none() {
                    self.expect_default_argument(scope, default, ty);
                }
            }
            if name != "_" {
                self.declare(scope, name, ty, false);
            }
        }
    }

    pub(super) fn selected_signature_default_function(&self, function: &FunDecl) -> bool {
        self.selected_signature_default_declarations
            .as_ref()
            .is_some_and(|selected| {
                selected.iter().any(|declaration| {
                    self.active_declarations
                        .function(self.file, *declaration)
                        .is_some_and(|candidate| std::ptr::eq(candidate, function))
                })
            })
    }

    pub(super) fn selected_signature_default_source_member(
        &self,
        member: crate::libraries::SourceMember,
    ) -> bool {
        self.active_declarations
            .source_member_declaration(self.file, self.resolved_index, member)
            .is_some_and(|declaration| {
                self.selected_signature_default_declarations
                    .as_ref()
                    .is_some_and(|selected| selected.contains(&declaration))
            })
    }

    pub(super) fn selected_signature_default_constructor(
        &self,
        class: DeclId,
        secondary: Option<usize>,
    ) -> bool {
        self.selected_signature_default_declarations
            .as_ref()
            .is_some_and(|selected| {
                let Some(classifier) = self.active_declarations.classifier_declaration(class)
                else {
                    return false;
                };
                selected.iter().any(|declaration| {
                    let Some(anchor) = self.resolved_index.declaration_anchor(*declaration) else {
                        return false;
                    };
                    anchor.kind == crate::fir::DeclarationKind::Constructor
                        && anchor.owner.is_some_and(|owner| {
                            owner == classifier
                                || self
                                    .active_declarations
                                    .same_parser_declaration(owner, classifier)
                        })
                        && anchor.sibling
                            == secondary.map_or(0, |index| {
                                u32::try_from(index + 1).expect("too many secondary constructors")
                            })
                })
            })
    }
}
