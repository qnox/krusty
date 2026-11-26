//! Lambdas with a written parameter that names no type and that no callee gives a function shape.
//!
//! Such a parameter cannot be inferred: it carries the error type rather than a guessed `Any`, and
//! the lambda is reported once the whole file is checked, if no later recheck under a selected
//! shape replaced its analysis. Standing alone it needs an explicit type; as the argument of a
//! bare callee type variable the call owes that variable's report as well.

use super::*;

/// The lambdas analysed without a shape for a written parameter, the lambdas a selected call
/// committed as its arguments, and the call and type variable each unshaped argument was bound to
/// as a bare parameter of its callee.
#[derive(Default)]
pub(super) struct UnshapedLambdas {
    lambdas: crate::fx_hash::HashSet<ExprId>,
    arguments: crate::fx_hash::HashSet<ExprId>,
    formals: crate::fx_hash::HashMap<ExprId, (ExprId, String)>,
}

/// The type of a written lambda parameter that names none: the error type where nothing can
/// infer it, `Any` for an anonymous or implicit one.
pub(super) fn untyped_ty(uninferrable: &[bool], index: usize) -> Ty {
    if uninferrable.get(index).copied().unwrap_or(false) {
        Ty::Error
    } else {
        Ty::obj("kotlin/Any")
    }
}

/// Each argument of a selected call with the index of the signature parameter it fills.
pub(super) fn argument_signature_parameters(
    selected: &crate::libraries::FunctionInfo,
    args: &[ExprId],
    argument_names: Option<&[Option<String>]>,
    trailing_lambda: bool,
) -> Option<Vec<(usize, ExprId)>> {
    let signature = selected.generic_sig.as_ref()?;
    let context_count = selected.context_count.min(selected.callable.params.len());
    let value_signature = selected.call_sig.suffix(context_count);
    let argument_parameters = call_argument_parameter_indices(
        args.len(),
        signature.params.len().saturating_sub(context_count),
        argument_names,
        trailing_lambda,
        &value_signature,
    )?;
    Some(
        argument_parameters
            .into_iter()
            .zip(args.iter().copied())
            .map(|(parameter, argument)| (context_count + parameter, argument))
            .collect(),
    )
}

/// How a lambda argument stands to the selected parameter it fills.
pub(super) struct LambdaParameterBinding {
    pub(super) call: ExprId,
    pub(super) argument: ExprId,
    /// The argument's type from its probe, and the parameter's type the selection expects.
    pub(super) probed_actual: Ty,
    pub(super) expected: Ty,
    /// This argument's parameter as declared (its element type for a vararg).
    pub(super) declared: Ty,
}

impl Checker<'_> {
    /// The written parameters of `lambda` that name no type, by position: the ones a callee must
    /// give a type to, or that nothing can.
    pub(super) fn untyped_named_lambda_parameters(&self, lambda: ExprId) -> Vec<bool> {
        let Expr::Lambda { params, .. } = self.file.expr(lambda) else {
            return Vec::new();
        };
        let declared = self.file.lambda_param_types.get(&lambda.0);
        let roles = self.file.lambda_parameter_roles.get(&lambda.0);
        (0..params.len())
            .map(|index| {
                let explicitly_typed = declared
                    .and_then(|types| types.get(index))
                    .is_some_and(Option::is_some);
                let named = roles
                    .and_then(|roles| roles.get(index))
                    .is_none_or(|role| *role == LambdaParameterRole::Named);
                named && !explicitly_typed
            })
            .collect()
    }

    /// Report each written parameter of `lambda` that names no type and has none to infer from:
    /// as a call argument its callee could not give it a shape; standing alone it never had one.
    pub(super) fn report_uninferrable_lambda_parameters(
        &mut self,
        lambda: ExprId,
        as_argument: bool,
    ) {
        let Expr::Lambda { params, .. } = self.file.expr(lambda) else {
            return;
        };
        let params = params.clone();
        let uninferrable = self.untyped_named_lambda_parameters(lambda);
        let spans = self.file.lambda_param_spans.get(&lambda.0);
        let mut errors = Vec::new();
        for (index, parameter) in params.iter().enumerate() {
            if !uninferrable[index] {
                continue;
            }
            let span = spans
                .and_then(|spans| spans.get(index))
                .copied()
                .unwrap_or_else(|| self.span(lambda));
            errors.push((
                span,
                if as_argument {
                    format!(
                        "cannot infer type for value parameter '{parameter}'. Specify it explicitly."
                    )
                } else {
                    "an explicit type is required on a value parameter.".to_string()
                },
            ));
        }
        for (span, message) in errors {
            self.diags.error(span, message);
        }
    }

    /// Report the lambdas whose analysis found no function shape for a written parameter and
    /// which no later recheck under a selected shape replaced: a probe's guess is not a
    /// diagnostic, so this runs once the whole file is checked.
    pub(super) fn report_unshaped_lambda_parameters(&mut self) {
        let mut lambdas = std::mem::take(&mut self.unshaped_lambdas.lambdas)
            .into_iter()
            .collect::<Vec<_>>();
        lambdas.sort_by_key(|lambda| lambda.0);
        for lambda in lambdas {
            let still_unshaped = matches!(
                self.expr_types[lambda.0 as usize].kind(),
                TyKind::Fun(signature) if signature.params.iter().any(|parameter| parameter.mentions_error())
            );
            if still_unshaped {
                if let Some((call, formal)) = self.unshaped_lambdas.formals.remove(&lambda) {
                    self.diags.error(
                        self.call_callee_name_span(call),
                        format!(
                            "cannot infer type for type parameter '{formal}'. Specify it explicitly."
                        ),
                    );
                }
                let as_argument = self.unshaped_lambdas.arguments.contains(&lambda);
                self.report_uninferrable_lambda_parameters(lambda, as_argument);
            }
        }
    }

    /// Analyse `lambda` for the written parameters it must give a type to and note it unshaped if
    /// any of them has none to take from a function shape: the lambda is reported at the end of
    /// the file, if no selected shape replaced this analysis. Returns those parameters by
    /// position.
    pub(super) fn note_unshaped_lambda(&mut self, lambda: ExprId) -> Vec<bool> {
        let uninferrable = self.untyped_named_lambda_parameters(lambda);
        if uninferrable.contains(&true) {
            self.unshaped_lambdas.lambdas.insert(lambda);
        }
        uninferrable
    }

    /// Record that a selected call committed `argument` to its parameter `declared` (the element
    /// type for a vararg). A lambda that stays unshaped is then reported as an argument its callee
    /// could not shape, and, when `declared` is a bare type variable of the callee that no explicit
    /// type argument gives, the call owes that variable's report as well.
    pub(super) fn record_committed_lambda_argument(
        &mut self,
        call: ExprId,
        argument: ExprId,
        declared: Ty,
    ) {
        if !matches!(self.file.expr(argument), Expr::Lambda { .. }) {
            return;
        }
        self.unshaped_lambdas.arguments.insert(argument);
        if !self.unshaped_lambdas.lambdas.contains(&argument)
            || self.file.call_type_args.contains_key(&call.0)
        {
            return;
        }
        if let TyKind::TyParam(formal, _) | TyKind::Var(formal, _) = declared.kind() {
            let name = crate::types::type_parameter_source_name(formal.as_str());
            self.unshaped_lambdas
                .formals
                .insert(argument, (call, name.to_string()));
        }
    }

    /// The declared parameter of a lambda argument whose selection left it no function shape. A
    /// lambda with a written parameter that names no type is probed without a shape, so its
    /// parameters carry the error type. When the selected parameter is a bare type variable and
    /// the selection bound it to exactly that probe, nothing else constrained the variable: the
    /// lambda is analysed as a standalone literal under the variable's own bound, and the call owes
    /// the variable's report. A shape another constraint supplied (the receiver's arguments, the
    /// expected type, another argument) names its parameters, so the selection never equals the
    /// probe.
    pub(super) fn lambda_bound_only_the_callee_variable(
        &mut self,
        binding: &LambdaParameterBinding,
    ) -> Option<Ty> {
        let LambdaParameterBinding {
            call,
            argument,
            probed_actual,
            expected,
            declared,
        } = *binding;
        let probe_left_a_parameter_untyped = matches!(
            probed_actual.kind(),
            TyKind::Fun(signature) if signature.params.iter().any(|parameter| parameter.mentions_error())
        );
        if !matches!(self.file.expr(argument), Expr::Lambda { .. })
            || !self
                .untyped_named_lambda_parameters(argument)
                .contains(&true)
            || !probe_left_a_parameter_untyped
            || expected != probed_actual
            || self.file.call_type_args.contains_key(&call.0)
            || !matches!(declared.kind(), TyKind::TyParam(..) | TyKind::Var(..))
        {
            return None;
        }
        // The probe analysed the lambda under no real shape; that analysis is void, so the
        // unshaped path owes it again.
        self.expr_lowers.remove(&argument);
        Some(declared)
    }
}
