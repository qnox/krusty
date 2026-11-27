//! Calls of inline library composables: which of a call's lambdas the callee inlines, and the facts
//! of a lambda whose body the call places in its caller.

use super::*;

/// How a call passes one of its lambda arguments ([`BodyScope::lambda_placement`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LambdaPlacement {
    /// The call inlines the lambda's body into its caller.
    Placed,
    /// The lambda of a `crossinline` parameter, which the callee may move into code of its own.
    Crossinline,
    /// The lambda is a function value the callee receives.
    Value,
}

impl BodyScope<'_> {
    /// Whether `lambda`, an argument of `call`, is placed in its caller.
    pub(super) fn places_lambda(&self, ir: &IrFile, call: ExprId, lambda: ExprId) -> bool {
        self.lambda_placement(ir, call, lambda) == Some(LambdaPlacement::Placed)
    }

    /// How `call` passes `lambda`, one of its arguments: a call of an inline library composable
    /// places the lambda of an inline parameter ([`crate::types::is_inline_parameter`]) in its
    /// caller, and passes any other lambda as a function value. `None` when the declaration
    /// published neither the parameter's modifier nor its declared type, so the call is refused.
    pub(super) fn lambda_placement(
        &self,
        ir: &IrFile,
        call: ExprId,
        lambda: ExprId,
    ) -> Option<LambdaPlacement> {
        let IrExpr::Call { callee, args, .. } = &ir.exprs[call as usize] else {
            return Some(LambdaPlacement::Value);
        };
        let Callee::External {
            target,
            extension_receiver_parameter,
            ..
        } = &**callee
        else {
            return Some(LambdaPlacement::Value);
        };
        let shape = self.source.checked.dependency_shape(*target)?;
        if !shape.inline || !matches!(ir.exprs[lambda as usize], IrExpr::Lambda { .. }) {
            return Some(LambdaPlacement::Value);
        }
        let (parameter, _) =
            supplied_arguments(callee, args).find(|&(_, argument)| argument == lambda)?;
        // The declaration's modifiers skip the extension receiver; its declared types do not.
        let value_parameter = match *extension_receiver_parameter {
            Some(receiver) if receiver == parameter => return Some(LambdaPlacement::Value),
            Some(receiver) if receiver < parameter => parameter - 1,
            _ => parameter,
        };
        let modifier = *shape.inline_modifiers.get(value_parameter as usize)?;
        let declared = *shape
            .declared_parameters
            .as_ref()?
            .get(parameter as usize)?;
        Some(if !crate::types::is_inline_parameter(modifier, declared) {
            LambdaPlacement::Value
        } else if modifier == crate::types::InlineParameterModifier::Crossinline {
            LambdaPlacement::Crossinline
        } else {
            LambdaPlacement::Placed
        })
    }

    /// The facts of the composable lambda `argument` an inline library composable places in its
    /// caller: its body is the template the call splices, and the composable calls in it, which
    /// also pass on the caller's own parameters through the values the lambda captures.
    pub(super) fn placed_lambda(
        &self,
        ir: &IrFile,
        argument: &LambdaArgument,
        identity: LambdaIdentity<'_>,
        direct_calls: &mut crate::fx_hash::HashSet<ExprId>,
        rejections: &mut Vec<PluginRejection>,
    ) -> Option<ComposableLambda> {
        let LambdaIdentity {
            key,
            memoization,
            caller_parameters,
            ..
        } = identity;
        let IrExpr::Lambda {
            captures,
            inline_body: Some(template),
            ..
        } = &ir.exprs[argument.lambda as usize]
        else {
            rejections.push(PluginRejection {
                span: self
                    .source
                    .expression_span(ir, argument.lambda)
                    .unwrap_or(crate::diag::Span::new(0, 0)),
                message: unsupported("an inline library composable's lambda without a template"),
                refusal: true,
            });
            return None;
        };
        let span = self
            .source
            .expression_span(ir, argument.lambda)
            .unwrap_or(crate::diag::Span::new(0, 0));
        // The standalone function the lambda was also lowered to holds the same calls; nothing
        // emits it.
        let mut pending = vec![*template];
        pending.extend(ir.functions[argument.implementation as usize].body);
        while let Some(expression) = pending.pop() {
            if self.calls.contains_key(&expression) {
                direct_calls.insert(expression);
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
        let mut reject = |what: &str| {
            rejections.push(PluginRejection {
                span,
                message: unsupported(what),
                refusal: true,
            });
        };
        let IrExpr::Block { stmts, .. } = &ir.exprs[*template as usize] else {
            reject("a composable lambda whose body is not a block");
            return None;
        };
        let direct = stmts
            .iter()
            .copied()
            .filter(|statement| self.calls.contains_key(statement))
            .collect::<Vec<_>>();
        if let Some(what) = unsupported_body_expression(ir, *template, &[], &[]) {
            reject(what);
            return None;
        }
        if stmts
            .first()
            .and_then(|&statement| self.source.expression_span(ir, statement))
            .is_none()
        {
            reject("an empty composable lambda");
            return None;
        }
        // The values the lambda takes of its own, its receiver first, follow its captures.
        let parameters = match lambda_parameters(
            ir,
            self.runtime,
            argument.implementation,
            *template,
            captures.len() as u32,
        ) {
            Ok(parameters) => parameters,
            Err(what) => {
                reject(what);
                return None;
            }
        };
        // A captured parameter of the caller is that parameter in the lambda's calls.
        let parameter_of_capture = |capture: u32| match captures
            .get(capture as usize)
            .map(|&value| &ir.exprs[value as usize])
        {
            Some(IrExpr::GetValue(parameter)) if *parameter < caller_parameters => Some(*parameter),
            _ => None,
        };
        let mut calls = Vec::with_capacity(direct.len());
        let mut locations = Vec::with_capacity(direct.len());
        for &call in &direct {
            if let Some((mut site, location)) = self.direct_call_site(
                ir,
                call,
                CallValues::Parameters(
                    ir.functions[argument.implementation as usize].params.len() as u32
                ),
                span,
                rejections,
            ) {
                for argument in &mut site.arguments {
                    if let ArgumentState::Forwarded(capture) = *argument {
                        *argument = parameter_of_capture(capture)
                            .map_or(ArgumentState::Uncertain, ArgumentState::Forwarded);
                    }
                }
                calls.push(site);
                locations.push(location);
            }
        }
        Some(ComposableLambda {
            lambda: argument.lambda,
            call: argument.call,
            memoization,
            captures: captures.len() as u32,
            parameters,
            implementation: argument.implementation,
            key,
            offsets: (0, 0),
            source_information: self
                .options
                .source_information
                .then(|| format!("C{}:{}", locations.join(","), self.file_reference)),
            trace: None,
            key_meta: false,
            opening_line: 0,
            calls,
            inline_template: Some(*template),
        })
    }
}
