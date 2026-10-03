//! Common adapters for checked function values.
//!
//! These adapters bind a receiver or forward `Function.invoke` using decisions already recorded in
//! FIR. Callable-reference identity and target-specific carrier realization stay separate.

use crate::ir::{IrCallableReference, IrCallableReferenceTarget, IrExpr, IrFunction};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

pub(super) struct CheckedFunctionInvokeReference<'a> {
    pub(super) callee: crate::fir::FirExprId,
    pub(super) target_parameters: &'a [crate::fir::ResolvedTy],
    pub(super) target_result: crate::fir::ResolvedTy,
    pub(super) target_suspend: bool,
    pub(super) reference_parameters: &'a [crate::fir::ResolvedTy],
    pub(super) reference_result: crate::fir::ResolvedTy,
    pub(super) suspend: bool,
    pub(super) reflective: bool,
}

impl BodyLowering<'_> {
    pub(super) fn checked_extension_function_binding(
        &mut self,
        receiver: crate::fir::FirReceiver,
        callable: crate::fir::FirExprId,
        target_parameters: &[crate::fir::ResolvedTy],
        receiver_parameter: u32,
        target_result: crate::fir::ResolvedTy,
        suspend: bool,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let fir_callable = callable;
        let receiver_parameter = usize::try_from(receiver_parameter)
            .map_err(|_| FirLoweringFailure::MissingExpression(callable))?;
        let receiver_type = target_parameters
            .get(receiver_parameter)
            .copied()
            .ok_or(FirLoweringFailure::MissingExpression(fir_callable))?;
        let callable_type = self
            .body
            .expr(callable)
            .ok_or(FirLoweringFailure::MissingExpression(callable))?
            .ty
            .get();

        // The parenthesized callee runs before the receiver: `getReceiver().(getFun())` logs
        // `getFun` then `getReceiver`. The wrapper's first two value slots follow that order.
        let callable_value = self.expression(fir_callable)?;
        let receiver_value =
            self.expression_with_conversion(receiver.value, receiver.conversion)?;
        let function_value = self.ir.add_expr(IrExpr::GetValue(0));
        let bound_receiver = self.ir.add_expr(IrExpr::GetValue(1));
        let mut next_parameter = 2u32;
        let arguments = target_parameters
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                if parameter == receiver_parameter {
                    bound_receiver
                } else {
                    let value = self.ir.add_expr(IrExpr::GetValue(next_parameter));
                    next_parameter = next_parameter
                        .checked_add(1)
                        .expect("too many bound extension-function parameters");
                    value
                }
            })
            .collect::<Vec<_>>();
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: function_value,
            args: arguments,
            params: target_parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            ret: target_result.get(),
        });
        if suspend {
            self.ir.suspend_calls.insert(invoke, target_result.get());
        }
        let body =
            self.callable_reference_adapter_body(invoke, target_result.get(), target_result.get());
        let bound_parameters = target_parameters
            .iter()
            .enumerate()
            .filter_map(|(parameter, ty)| (parameter != receiver_parameter).then_some(ty.get()))
            .collect::<Vec<_>>();
        let mut parameters = Vec::with_capacity(bound_parameters.len() + 2);
        parameters.push(callable_type);
        parameters.push(receiver_type.get());
        parameters.extend(bound_parameters.iter().copied());
        let wrapper = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_extension_bind_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: crate::types::stored_value_ty(target_result.get()),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir
            .set_method_visibility(wrapper, crate::types::Visibility::Private);
        self.ir.lambda_own_params_from.insert(wrapper, 2);
        if suspend {
            self.ir.suspend_funs.push(wrapper);
        }
        self.attach_generated_static_to_lexical_class(wrapper);
        let arity = bound_parameters
            .len()
            .checked_add(usize::from(suspend))
            .and_then(|arity| u8::try_from(arity).ok())
            .ok_or(FirLoweringFailure::MissingExpression(fir_callable))?;
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: wrapper,
            arity,
            captures: vec![callable_value, receiver_value],
            sam: None,
            inline_body: None,
        }))
    }

    pub(super) fn checked_function_invoke_reference(
        &mut self,
        reference: CheckedFunctionInvokeReference<'_>,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let CheckedFunctionInvokeReference {
            callee,
            target_parameters,
            target_result,
            target_suspend,
            reference_parameters,
            reference_result,
            suspend,
            reflective,
        } = reference;
        if target_parameters.len() != reference_parameters.len() || (target_suspend && !suspend) {
            return Err(FirLoweringFailure::MissingExpression(callee));
        }
        let callee_type = self
            .body
            .expr(callee)
            .ok_or(FirLoweringFailure::MissingExpression(callee))?
            .ty
            .get();
        let captured = self.expression(callee)?;
        let function_value = self.ir.add_expr(IrExpr::GetValue(0));
        let arguments = reference_parameters
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                self.ir.add_expr(IrExpr::GetValue(
                    u32::try_from(parameter + 1).expect("too many invoke-reference parameters"),
                ))
            })
            .collect::<Vec<_>>();
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: function_value,
            args: arguments,
            params: target_parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            ret: target_result.get(),
        });
        if target_suspend {
            self.ir.suspend_calls.insert(invoke, target_result.get());
        }
        let body = self.callable_reference_adapter_body(
            invoke,
            target_result.get(),
            reference_result.get(),
        );
        let mut parameters = Vec::with_capacity(reference_parameters.len() + 1);
        parameters.push(callee_type);
        parameters.extend(reference_parameters.iter().map(|parameter| parameter.get()));
        let wrapper = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_invoke_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: crate::types::stored_value_ty(reference_result.get()),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir
            .set_method_visibility(wrapper, crate::types::Visibility::Private);
        self.ir.lambda_own_params_from.insert(wrapper, 1);
        if suspend {
            self.ir.suspend_funs.push(wrapper);
        }
        self.attach_generated_static_to_lexical_class(wrapper);
        let arity = reference_parameters
            .len()
            .checked_add(usize::from(suspend))
            .and_then(|arity| u8::try_from(arity).ok())
            .ok_or(FirLoweringFailure::MissingExpression(callee))?;
        if reflective {
            let function_type = if suspend {
                Ty::fun_suspend(
                    reference_parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    reference_result.get(),
                )
            } else {
                Ty::fun(
                    reference_parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    reference_result.get(),
                )
            };
            Ok(self
                .ir
                .add_expr(IrExpr::CallableReference(IrCallableReference {
                    target: IrCallableReferenceTarget::FunctionInvoke,
                    adapter: wrapper,
                    captures: Vec::new(),
                    bound_receiver: Some(captured),
                    function_type,
                    declaration_parameters: target_parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    declaration_result: target_result.get(),
                    declaration_suspend: target_suspend,
                    adaptation: None,
                })))
        } else {
            Ok(self.ir.add_expr(IrExpr::Lambda {
                impl_fn: wrapper,
                arity,
                captures: vec![captured],
                sam: None,
                inline_body: None,
            }))
        }
    }
}
