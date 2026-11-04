//! Publish selected callable-reference targets and their declaration-owned diagnostic evidence.

use super::*;

/// Diagnostic projection of the current selected declaration, before invocation adaptation.
/// Kept only by the checker: no backend reconstructs a declaration from this rendering.
#[derive(Clone)]
pub(super) struct SelectedReferenceDiagnostic {
    pub(super) span: Span,
    candidate: String,
    kind: ReferenceConstraintDiagnostic,
}

#[derive(Clone, Copy)]
enum ReferenceConstraintDiagnostic {
    Callable,
    ExtensionReceiver,
}

impl SelectedReferenceDiagnostic {
    pub(super) fn into_message(self) -> String {
        match self.kind {
            ReferenceConstraintDiagnostic::Callable => {
                format!("inapplicable candidate(s): {}", self.candidate)
            }
            ReferenceConstraintDiagnostic::ExtensionReceiver => {
                crate::diagnostic_wording::callable_reference_receiver_mismatch(&self.candidate)
            }
        }
    }
}

impl Checker<'_> {
    pub(super) fn record_reference_diagnostic(&mut self, expression: ExprId, candidate: String) {
        self.publish_reference_diagnostic(
            expression,
            candidate,
            ReferenceConstraintDiagnostic::Callable,
        );
    }

    pub(super) fn record_function_reference_diagnostic(
        &mut self,
        expression: ExprId,
        function: &crate::libraries::FunctionInfo,
    ) {
        let kind = if function.is_extension() {
            ReferenceConstraintDiagnostic::ExtensionReceiver
        } else {
            ReferenceConstraintDiagnostic::Callable
        };
        self.publish_reference_diagnostic(
            expression,
            Self::callable_candidate_display(&function.callable.name, function),
            kind,
        );
    }

    fn publish_reference_diagnostic(
        &mut self,
        expression: ExprId,
        candidate: String,
        kind: ReferenceConstraintDiagnostic,
    ) {
        let Expr::CallableRef { name, .. } = self.file.expr(expression) else {
            unreachable!("a selected reference diagnostic belongs to a callable reference")
        };
        let span = self.member_name_span(expression, name);
        self.selected_callable_reference_diagnostics.insert(
            expression,
            SelectedReferenceDiagnostic {
                span,
                candidate,
                kind,
            },
        );
    }

    pub(super) fn record_local_reference_diagnostic(
        &mut self,
        expression: ExprId,
        statement: StmtId,
        ret: Ty,
    ) {
        let Stmt::LocalFun(function) = self.file.stmt(statement) else {
            unreachable!("a selected local function reference retains its declaration")
        };
        let candidate = source_function_display(self.file, function, ret);
        let kind = if function.receiver.is_some() {
            ReferenceConstraintDiagnostic::ExtensionReceiver
        } else {
            ReferenceConstraintDiagnostic::Callable
        };
        self.publish_reference_diagnostic(expression, candidate, kind);
    }

    pub(super) fn record_top_level_function_ref(
        &mut self,
        expression: ExprId,
        name: &str,
        selected: &crate::libraries::FunctionInfo,
        ty: Ty,
    ) -> Ty {
        let target = selected.callable.clone();
        if let Some(source_key) = selected.source_key {
            self.resolved_source_calls.insert(expression, source_key);
        }
        match (
            selected.stable_declaration,
            selected.source_key,
            &target.origin,
        ) {
            (Some(_), _, Origin::Module { .. }) => {}
            (None, Some(_), Origin::Module { .. }) => {}
            (None, None, Origin::Library) if target.external_identity.is_some() => {}
            _ => {
                self.diags.error(
                    self.member_name_span(expression, name),
                    format!(
                        "cannot reference '{name}': the selected callable has no stable identity"
                    ),
                );
                return Ty::Error;
            }
        }
        self.record_function_reference_diagnostic(expression, selected);
        self.expr_lowers.insert(
            expression,
            ExprLowering::TopLevelFunctionRef(Box::new(ResolvedTopLevelFunctionRef {
                target,
                stable_declaration: selected.stable_declaration,
            })),
        );
        ty
    }

    pub(super) fn record_adapted_top_level_function_ref(
        &mut self,
        expression: ExprId,
        name: &str,
        selected: &crate::libraries::FunctionInfo,
        expected: &'static crate::types::FnSig,
        vararg_collect: Option<usize>,
        argument_mapping: Option<Vec<AdaptedRefArgument>>,
    ) -> Ty {
        let resolved_argument_mapping = self
            .callable_ref_argument_plan(selected, expected)
            .expect("a selected adapted reference must retain its complete argument plan");
        let target = selected.callable.clone();
        if let Some(source_key) = selected.source_key {
            self.resolved_source_calls.insert(expression, source_key);
        }
        let target_default_values = match (
            selected.stable_declaration,
            selected.source_key,
            &target.origin,
        ) {
            (Some(_), _, Origin::Module { .. }) => selected.default_values.clone(),
            (None, Some(_), Origin::Module { .. }) => selected.default_values.clone(),
            (None, None, Origin::Library) if target.external_identity.is_some() => Vec::new(),
            _ => {
                self.diags.error(
                    self.member_name_span(expression, name),
                    format!(
                        "cannot reference '{name}': the selected callable has no stable identity"
                    ),
                );
                return Ty::Error;
            }
        };
        self.record_function_reference_diagnostic(expression, selected);
        self.expr_lowers.insert(
            expression,
            ExprLowering::AdaptedRef {
                target: Box::new(target),
                stable_declaration: selected.stable_declaration,
                target_default_values,
                adapted_params: expected.params.clone(),
                ret: expected.ret,
                vararg_tail: selected.call_sig.vararg
                    && expected.params.len() + 1 == selected.callable.params.len(),
                target_vararg: selected.call_sig.vararg,
                vararg_collect,
                argument_mapping,
                resolved_argument_mapping,
                suspend_conversion: expected.suspend && !selected.callable.suspend,
            },
        );
        Ty::from(TyKind::Fun(expected))
    }
}
