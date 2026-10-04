//! Lowering of frontend-selected functional-interface conversions.

use crate::fir::FirSamConversion;
use crate::ir::{ExprId, IrExpr, IrFunction, IrSamTarget};
use crate::types::Ty;

pub(super) fn ir_sam_method(method: crate::fir::FirSamMethod) -> crate::ir::IrSamMethod {
    match method {
        crate::fir::FirSamMethod::Declared(crate::fir::ResolvedFunctionOverrideTarget::Module(
            callable,
        )) => crate::ir::IrSamMethod::Module(callable),
        crate::fir::FirSamMethod::Declared(
            crate::fir::ResolvedFunctionOverrideTarget::External(callable),
        ) => crate::ir::IrSamMethod::External(callable),
        crate::fir::FirSamMethod::FunctionTypeInvoke => crate::ir::IrSamMethod::FunctionTypeInvoke,
    }
}

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
        let function_adapter = matches!(self.ir.expr(function), IrExpr::CallableReference(_));
        let arity = u8::try_from(conversion.parameters.len()).ok()?;
        // The captured value keeps its own function arity. A non-suspend value adapted to a
        // suspend method is `FunctionN`; the implementation still receives the continuation.
        let captured_suspend = conversion.suspend && conversion.source_suspend;
        let function_type = Ty::fun_with_shape(
            conversion
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            conversion.result.get(),
            conversion.context_count as usize,
            conversion.has_receiver,
            captured_suspend,
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
        // The captured suspend value takes this adapter's continuation as its last argument.
        // A non-suspend value adapted to a suspend method does not: that call stays `FunctionN`.
        if captured_suspend {
            self.ir
                .suspend_calls
                .insert(invoke, conversion.result.get());
        }
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
        self.ir
            .set_method_visibility(implementation, crate::types::Visibility::Private);
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
                    method_target: ir_sam_method(conversion.method_target),
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
                    source_suspend: conversion.source_suspend,
                    overrides_non_primitive_result: conversion.overrides_non_primitive_result,
                    overridden_non_primitive_results: conversion
                        .overridden_non_primitive_results
                        .iter()
                        .map(|result| result.get())
                        .collect(),
                    function_adapter,
                    wraps_function_value: !function_adapter,
                    nullable: conversion.nullable,
                    kotlin_interface: conversion.kotlin_interface,
                    parameter_identities: conversion.parameter_identities.to_vec(),
                }),
                inline_body: None,
            }),
        )
    }
}
