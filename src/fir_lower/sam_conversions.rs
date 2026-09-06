//! Lowering of frontend-selected functional-interface conversions.

use crate::fir::FirSamConversion;
use crate::ir::{ExprId, IrExpr, IrFunction, IrSamTarget};
use crate::types::Ty;

use super::BodyLowering;

impl BodyLowering<'_> {
    /// Adapt an already-materialized function value to the selected SAM declaration. The generated
    /// implementation captures that value and forwards the interface method's checked arguments to
    /// `FunctionN.invoke`; target representation remains a backend decision.
    pub(super) fn sam_function_value_adapter(
        &mut self,
        conversion: &FirSamConversion,
        function: ExprId,
    ) -> Option<ExprId> {
        let function_adapter = match self.ir.expr(function) {
            IrExpr::New { internal, .. } => self
                .ir
                .classes
                .iter()
                .any(|class| class.fq_name_id() == *internal && class.func_ref.is_some()),
            IrExpr::StaticInstance { owner, .. } => self
                .ir
                .classes
                .get(*owner as usize)
                .is_some_and(|class| class.func_ref.is_some()),
            _ => false,
        };
        let arity = u8::try_from(conversion.parameters.len()).ok()?;
        let function_type = Ty::fun_with_shape(
            conversion
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            conversion.result.get(),
            conversion.context_count as usize,
            conversion.has_receiver,
            conversion.suspend,
        );
        let callee = self.ir.add_expr(IrExpr::GetValue(0));
        let arguments = conversion
            .parameters
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                self.ir.add_expr(IrExpr::GetValue(
                    u32::try_from(parameter + 1).expect("too many SAM parameters"),
                ))
            })
            .collect::<Vec<_>>();
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: callee,
            args: arguments,
            params: conversion
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            ret: conversion.result.get(),
        });
        let body = self.callable_reference_adapter_body(
            invoke,
            conversion.result.get(),
            conversion.result.get(),
        );
        let mut parameters = Vec::with_capacity(conversion.parameters.len() + 1);
        parameters.push(function_type);
        parameters.extend(
            conversion
                .parameters
                .iter()
                .map(|parameter| parameter.get()),
        );
        let implementation = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_sam_delegate_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: crate::types::stored_value_ty(conversion.result.get()),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir.private_methods.insert(implementation);
        self.ir.lambda_own_params_from.insert(implementation, 1);
        self.ir.lambda_sam_signature.insert(
            implementation,
            (
                conversion
                    .declared_parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                conversion.declared_result.get(),
            ),
        );
        if conversion.suspend {
            self.ir.suspend_funs.push(implementation);
        }
        Some(
            self.ir.add_expr(IrExpr::Lambda {
                impl_fn: implementation,
                arity,
                captures: vec![function],
                sam: Some(IrSamTarget {
                    classifier: conversion.classifier,
                    method: conversion.method.to_string(),
                    parameters: conversion
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    result: conversion.result.get(),
                    declared_parameters: conversion
                        .declared_parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    declared_result: conversion.declared_result.get(),
                    context_count: conversion.context_count,
                    has_receiver: conversion.has_receiver,
                    suspend: conversion.suspend,
                    function_adapter,
                }),
                inline_body: None,
            }),
        )
    }
}
