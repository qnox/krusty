//! Lowering of frontend-selected regular-function to suspend-function conversions.

use crate::fir::ResolvedTy;
use crate::ir::{ExprId, IrExpr, IrFunction};
use crate::types::{stored_value_ty, Ty};

use super::BodyLowering;

impl BodyLowering<'_> {
    /// Wrap an already-materialized regular function value in a suspend forwarding closure.
    /// `from` and `to` are complete checked callable shapes; this routine performs no selection or
    /// assignability decision.
    pub(super) fn suspend_function_value_adapter(
        &mut self,
        from: ResolvedTy,
        to: ResolvedTy,
        function: ExprId,
    ) -> Option<ExprId> {
        let (Ty::Fun(source), Ty::Fun(target)) = (from.get().non_null(), to.get().non_null())
        else {
            return None;
        };
        if source.suspend || !target.suspend || source.params.len() != target.params.len() {
            return None;
        }

        let callee = self.ir.add_expr(IrExpr::GetValue(0));
        let arguments = target
            .params
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                self.ir.add_expr(IrExpr::GetValue(
                    u32::try_from(parameter + 1).expect("too many suspend-conversion parameters"),
                ))
            })
            .collect::<Vec<_>>();
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: callee,
            args: arguments,
            params: source.params.clone(),
            ret: source.ret,
        });
        let body = self.callable_reference_adapter_body(invoke, source.ret, target.ret);
        let mut parameters = Vec::with_capacity(target.params.len() + 1);
        parameters.push(from.get());
        parameters.extend(target.params.iter().copied());
        let implementation = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_suspend_delegate_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: stored_value_ty(target.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir.private_methods.insert(implementation);
        self.ir.lambda_own_params_from.insert(implementation, 1);
        self.ir.suspend_funs.push(implementation);
        let arity = target
            .params
            .len()
            .checked_add(1)
            .and_then(|arity| u8::try_from(arity).ok())?;
        Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity,
            captures: vec![function],
            sam: None,
            inline_body: None,
        }))
    }
}
