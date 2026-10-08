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
        // Only a Kotlin fun interface promises FunctionAdapter equality for a callable reference.
        // A Java SAM keeps the ordinary indy conversion kotlinc emits.
        let function_adapter = conversion.kotlin_interface
            && matches!(self.ir.expr(function), IrExpr::CallableReference(_));
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
        // Common IR keeps the checked function's logical parameter types. A target that needs an
        // erased adapter for a contravariant capture realizes that physical boundary later.
        let parameters = conversion
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let void_method =
            !conversion.suspend && conversion.declared_result.get() == crate::types::Ty::Unit;
        let callee = self.ir.add_expr(IrExpr::GetValue(0));
        let arguments = parameters
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
            params: parameters.clone(),
            ret: conversion.result.get(),
        });
        // The captured suspend value takes this adapter's continuation as its last argument.
        // A non-suspend value adapted to a suspend method does not: that call stays `FunctionN`.
        if captured_suspend {
            self.ir
                .suspend_calls
                .insert(invoke, conversion.result.get());
        }
        let body = if void_method {
            let done = self.ir.add_expr(IrExpr::Return(None));
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![invoke, done],
                value: None,
            })
        } else {
            self.callable_reference_adapter_body(
                invoke,
                conversion.result.get(),
                conversion.result.get(),
            )
        };
        let mut implementation_parameters = Vec::with_capacity(parameters.len() + 1);
        implementation_parameters.push(function_type);
        implementation_parameters.extend(parameters);
        let implementation = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_sam_delegate_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: implementation_parameters,
            ret: if void_method {
                crate::types::Ty::Unit
            } else {
                crate::types::stored_value_ty(conversion.result.get())
            },
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut parameter_identities = Vec::with_capacity(conversion.parameters.len() + 1);
        parameter_identities.push(crate::ir::IrParameterIdentity::generated(
            crate::ir::IrGeneratedParameterRole::SamAdapterCapture { ordinal: 0 },
            None,
        ));
        parameter_identities.extend(conversion.parameters.iter().enumerate().map(
            |(ordinal, _)| {
                crate::ir::IrParameterIdentity::generated(
                    crate::ir::IrGeneratedParameterRole::SamAdapterValue {
                        ordinal: u32::try_from(ordinal).expect("too many SAM parameters"),
                    },
                    None,
                )
            },
        ));
        self.ir.fn_params.insert(
            implementation,
            crate::ir::FnParamInfo::identities(parameter_identities),
        );
        if let Some(&line) = self.ir.expr_source_lines.get(&function) {
            self.ir.fn_decl_lines.insert(implementation, line);
        }
        self.record_sam_adapter_origin(implementation);
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
                    contravariant_parameters: conversion.contravariant_parameters.to_vec(),
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
                    overridden_results: conversion
                        .overridden_results
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

    /// Name the adapter as the next `$lambda$N` of the enclosing callable. kotlinc counts it in
    /// the same sequence as a source lambda written at the conversion.
    fn record_sam_adapter_origin(&mut self, function: crate::ir::FunId) {
        let implementation_name = self.body.debug_name().unwrap_or_default().to_owned();
        let lexical_owner = self.sam_adapter_lexical_owner();
        let next = |select: &dyn Fn(&crate::ir::IrLambdaOrigin) -> Option<u32>| {
            self.ir
                .lambda_origins
                .values()
                .filter_map(select)
                .max()
                .map_or(0, |ordinal| ordinal.saturating_add(1))
        };
        let identity = next(&|origin| Some(origin.identity));
        let implementation_ordinal = next(&|origin| {
            (origin.lexical_owner == lexical_owner
                && origin.implementation_name == implementation_name)
                .then_some(origin.implementation_ordinal)
        });
        self.ir.lambda_origins.insert(
            function,
            crate::ir::IrLambdaOrigin {
                identity,
                lexical_owner,
                enclosing_name: implementation_name.clone(),
                binding_name: None,
                ordinal: implementation_ordinal,
                implementation_name,
                implementation_ordinal,
                receiver_parameter: None,
                label: None,
                // kotlinc emits this adapter as a lambda method, which carries no generic
                // `Signature`. A literal origin is that method. It records no value-parameter
                // identities, so the null checks a source lambda writes for its own parameters
                // are not applied to arguments this method only forwards.
                form: crate::ir::IrLambdaForm::Literal,
                explicit_suspend: false,
                class_provenance: None,
            },
        );
    }

    fn sam_adapter_lexical_owner(&self) -> Option<crate::types::TypeName> {
        let owner = self.body.lexical_class_owner()?;
        let class = self
            .ir
            .checked_classifier_classes
            .get(&owner)
            .or_else(|| self.ir.checked_enum_entry_classes.get(&owner))
            .copied()?;
        Some(self.ir.classes[class as usize].fq_name_id())
    }
}
