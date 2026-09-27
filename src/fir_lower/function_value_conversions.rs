//! Lowering of frontend-selected function-value conversions.

use crate::fir::ResolvedTy;
use crate::ir::{ExprId, IrCallableReference, IrCallableReferenceTarget, IrExpr, IrFunction};
use crate::types::{stored_value_ty, Ty};

use super::BodyLowering;

impl BodyLowering<'_> {
    /// Realize the conversion of an already-materialized regular function value to the suspend
    /// and/or `Unit`-returning function type `to` as a callable reference bound to that value. Its
    /// adapter takes the value, then the target's parameters, invokes the value and coerces the
    /// result to the target's. `from` and `to` are complete checked callable shapes and `ordinal`
    /// the conversion's checked place in its callable; this routine selects nothing.
    pub(super) fn function_value_conversion(
        &mut self,
        from: ResolvedTy,
        to: ResolvedTy,
        ordinal: u32,
        value: ExprId,
    ) -> Option<ExprId> {
        let (Ty::Fun(source), Ty::Fun(target)) = (from.get().non_null(), to.get().non_null())
        else {
            return None;
        };
        if source.suspend || source.params.len() != target.params.len() {
            return None;
        }

        let callee = self.ir.add_expr(IrExpr::GetValue(0));
        let arguments = (1..=target.params.len())
            .map(|parameter| {
                self.ir.add_expr(IrExpr::GetValue(
                    u32::try_from(parameter).expect("too many converted-function parameters"),
                ))
            })
            .collect::<Vec<_>>();
        // The adapter only discards the value's result or hands it on as a suspend function's
        // erased result, so it reads the `FunctionN.invoke` result as the object it returns.
        let erased_result = Ty::nullable(Ty::obj("kotlin/Any"));
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: callee,
            args: arguments,
            params: source.params.clone(),
            ret: erased_result,
        });
        let adapter_result = if target.ret == Ty::Unit {
            Ty::Unit
        } else {
            erased_result
        };
        let body = self.callable_reference_adapter_body(invoke, erased_result, adapter_result);
        let mut parameters = Vec::with_capacity(target.params.len() + 1);
        parameters.push(from.get());
        parameters.extend(target.params.iter().copied());
        let adapter = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_function_value_conversion_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters.clone(),
            ret: stored_value_ty(target.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        if target.suspend {
            self.ir.suspend_funs.push(adapter);
        }
        self.ir.private_methods.insert(adapter);
        // The converted value is the adapter's bound receiver, ahead of its own parameters.
        self.ir.lambda_own_params_from.insert(adapter, 1);
        self.attach_generated_static_to_lexical_class(adapter);
        Some(
            self.ir
                .add_expr(IrExpr::CallableReference(IrCallableReference {
                    target: IrCallableReferenceTarget::FunctionValueConversion { ordinal },
                    adapter,
                    captures: Vec::new(),
                    bound_receiver: Some(value),
                    function_type: to.get(),
                    declaration_parameters: parameters.into_boxed_slice(),
                    declaration_result: target.ret,
                    declaration_suspend: target.suspend,
                    adaptation: None,
                })),
        )
    }
}
