//! Common IR construction of an adapter around a checked function value.

use super::{ExprId, FunId, IrExpr, IrFile, IrFunction, IrSamTarget};
use crate::types::Ty;

impl IrFile {
    /// Wrap a materialized function value in its already-selected functional-interface contract.
    /// Declaration selection and phase-specific naming/provenance remain with the caller.
    pub(crate) fn sam_function_value_adapter(
        &mut self,
        mut target: IrSamTarget,
        function: ExprId,
        name: String,
    ) -> Option<(FunId, ExprId)> {
        // Only a Kotlin fun interface promises FunctionAdapter equality for a callable reference.
        // A Java SAM keeps the ordinary indy conversion kotlinc emits.
        let function_adapter =
            target.kotlin_interface && matches!(self.expr(function), IrExpr::CallableReference(_));
        target.function_adapter = function_adapter;
        target.wraps_function_value = !function_adapter;
        let arity = u8::try_from(target.parameters.len()).ok()?;
        // The captured value keeps its own function arity. A non-suspend value adapted to a
        // suspend method is `FunctionN`; the implementation still receives the continuation.
        let captured_suspend = target.suspend && target.source_suspend;
        let function_type = Ty::fun_with_shape(
            target.parameters.clone(),
            target.result,
            target.context_count as usize,
            target.has_receiver,
            captured_suspend,
        );
        // Common IR keeps the checked function's logical parameter types. A target that needs an
        // erased adapter for a contravariant capture realizes that physical boundary later.
        let parameters = target.parameters.clone();
        let void_method = !target.suspend && target.declared_result == crate::types::Ty::Unit;
        let callee = self.add_expr(IrExpr::GetValue(0));
        let arguments = parameters
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                self.add_expr(IrExpr::GetValue(
                    u32::try_from(parameter + 1).expect("too many SAM parameters"),
                ))
            })
            .collect::<Vec<_>>();
        let invoke = self.add_expr(IrExpr::InvokeFunction {
            func: callee,
            args: arguments,
            params: parameters.clone(),
            ret: target.result,
        });
        // The captured suspend value takes this adapter's continuation as its last argument.
        // A non-suspend value adapted to a suspend method does not: that call stays `FunctionN`.
        if captured_suspend {
            self.suspend_calls.insert(invoke, target.result);
        }
        let body = if void_method {
            let done = self.add_expr(IrExpr::Return(None));
            self.add_expr(IrExpr::Block {
                stmts: vec![invoke, done],
                value: None,
            })
        } else {
            let result = if target.result == Ty::Unit {
                let unit = self.add_expr(IrExpr::UnitInstance);
                self.add_expr(IrExpr::Block {
                    stmts: vec![invoke],
                    value: Some(unit),
                })
            } else {
                invoke
            };
            let returned = self.add_expr(IrExpr::Return(Some(result)));
            let body = self.add_expr(IrExpr::Block {
                stmts: vec![returned],
                value: None,
            });
            self.callable_reference_adapter_results.insert(body, result);
            body
        };
        let mut implementation_parameters = Vec::with_capacity(parameters.len() + 1);
        implementation_parameters.push(function_type);
        implementation_parameters.extend(parameters);
        let implementation = self.add_fun(IrFunction {
            name,
            params: implementation_parameters,
            ret: if void_method {
                crate::types::Ty::Unit
            } else {
                crate::types::stored_value_ty(target.result)
            },
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut parameter_identities = Vec::with_capacity(target.parameters.len() + 1);
        parameter_identities.push(crate::ir::IrParameterIdentity::generated(
            crate::ir::IrGeneratedParameterRole::SamAdapterCapture { ordinal: 0 },
            None,
        ));
        parameter_identities.extend(target.parameters.iter().enumerate().map(|(ordinal, _)| {
            crate::ir::IrParameterIdentity::generated(
                crate::ir::IrGeneratedParameterRole::SamAdapterValue {
                    ordinal: u32::try_from(ordinal).expect("too many SAM parameters"),
                },
                None,
            )
        }));
        self.fn_params.insert(
            implementation,
            crate::ir::FnParamInfo::identities(parameter_identities),
        );
        if let Some(line) = self.source_line(function) {
            self.function_position_mut(implementation).decl_line = Some(line);
        }
        self.set_method_visibility(implementation, crate::types::Visibility::Private);
        self.lambda_own_params_from.insert(implementation, 1);
        self.lambda_sam_signature.insert(
            implementation,
            (target.declared_parameters.clone(), target.declared_result),
        );
        if target.suspend {
            self.suspend_funs.push(implementation);
        }
        let expression = self.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity,
            captures: vec![function],
            sam: Some(Box::new(target)),
            inline_body: None,
        });
        Some((implementation, expression))
    }
}
