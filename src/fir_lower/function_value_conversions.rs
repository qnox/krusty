//! Lowering of frontend-selected function-value conversions.

use crate::fir::ResolvedTy;
use crate::ir::{ExprId, IrCallableReference, IrCallableReferenceTarget, IrExpr, IrFunction};
use crate::types::{stored_value_ty, Ty, TyKind};

use super::BodyLowering;

/// A converted function value: the value the conversion yields, and the callable reference to its
/// adapter, which compiles to the class the checker named.
pub(super) struct ConvertedFunctionValue {
    pub(super) value: ExprId,
    pub(super) reference: ExprId,
}

impl BodyLowering<'_> {
    /// Realize the conversion of an already-materialized function value to the suspend and/or
    /// `Unit`-returning function type `to` as a callable reference bound to that value. Its adapter
    /// takes the value, then the target's parameters, invokes the value (suspending when the value
    /// is itself `suspend`) and coerces the result to the target's. `from` and `to` are complete checked callable shapes and `ordinal`
    /// the conversion's checked place in its callable; this routine selects nothing.
    pub(super) fn function_value_conversion(
        &mut self,
        from: ResolvedTy,
        to: ResolvedTy,
        ordinal: u32,
        keeps_null: bool,
        value: ExprId,
    ) -> Option<ConvertedFunctionValue> {
        let (from, to) = (from.get(), to.get());
        let (TyKind::Fun(source), TyKind::Fun(target)) =
            (from.non_null().kind(), to.non_null().kind())
        else {
            return None;
        };
        // Kind conversion only adds suspension; the frontend never selects the reverse.
        if (source.suspend && !target.suspend) || source.params.len() != target.params.len() {
            return None;
        }
        if !keeps_null {
            let reference = self.function_value_adapter(from, to, ordinal, value);
            return Some(ConvertedFunctionValue {
                value: reference,
                reference,
            });
        }
        // A `null` value stays `null`; only a present one is converted:
        // `if (value != null) adapter(value) else null`. A read of a stable binding is its own
        // snapshot; any other value is held in a temporary.
        let stable = self.stable_value_read(value);
        let (stored, slot) = match stable {
            Some(slot) => (None, slot),
            None => {
                let (stored, temporary) = self.value_temporary(value, from);
                (Some(stored), temporary)
            }
        };
        let tested = match stable {
            Some(_) => value,
            None => self.checked_value_read(slot, false),
        };
        let read = self.checked_value_read(slot, stable.is_some());
        let present = self.ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg: read,
            type_operand: from.non_null(),
        });
        let reference =
            self.function_value_adapter(from.non_null(), to.non_null(), ordinal, present);
        let null = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let condition = self.ir.add_expr(IrExpr::primitive_bin_op(
            crate::ir::IrBinOp::Ne,
            tested,
            null,
        ));
        let absent = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let guarded = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), reference), (None, absent)],
        });
        self.ir.whens.exhaustive.insert(guarded, to);
        self.ir.set_logical_type(guarded, to);
        let value = match stored {
            Some(stored) => self.ir.add_expr(IrExpr::Block {
                stmts: vec![stored],
                value: Some(guarded),
            }),
            None => guarded,
        };
        Some(ConvertedFunctionValue { value, reference })
    }

    /// The callable reference to a generated adapter that converts `value`, of function type
    /// `from`, to the function type `to` that has the same parameters.
    fn function_value_adapter(&mut self, from: Ty, to: Ty, ordinal: u32, value: ExprId) -> ExprId {
        let (TyKind::Fun(source), TyKind::Fun(target)) =
            (from.non_null().kind(), to.non_null().kind())
        else {
            unreachable!("a function-value conversion converts between function types");
        };

        // A nullable value is invoked through a safe call on it, so a `null` value converts to an
        // adapter that yields `null` (`Unit` for a `Unit` target) instead of invoking it. Only a
        // conversion that does not keep `null` converts a nullable value.
        let enclosing_temporary = self.next_temporary;
        let guard = from.is_nullable().then(|| {
            self.next_temporary = u32::try_from(target.params.len() + 1)
                .expect("too many converted-function parameters");
            let value = self.ir.add_expr(IrExpr::GetValue(0));
            self.value_temporary(value, from)
        });
        let callee = match guard {
            Some((_, temporary)) => {
                let read = self.ir.add_expr(IrExpr::GetValue(temporary));
                self.ir.add_expr(IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg: read,
                    type_operand: from.non_null(),
                })
            }
            None => self.ir.add_expr(IrExpr::GetValue(0)),
        };
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
        // An already-suspend value is invoked as a suspension point of the suspend adapter, which
        // hands it the adapter's continuation.
        if source.suspend {
            self.ir.suspend_calls.insert(invoke, source.ret);
        }
        let invoke = match guard {
            Some((variable, temporary)) => {
                self.next_temporary = enclosing_temporary;
                // The safe call yields the value's own result, or `null`. A `Unit` adapter
                // discards it, so it reads no particular type.
                let (invoke, result) = if target.ret == Ty::Unit {
                    (invoke, erased_result)
                } else {
                    let result = Ty::nullable(source.ret);
                    let read = self.ir.add_expr(IrExpr::TypeOp {
                        op: crate::ir::IrTypeOp::ImplicitCoercion,
                        arg: invoke,
                        type_operand: result,
                    });
                    (read, result)
                };
                self.null_guarded_invoke(variable, temporary, invoke, result)
            }
            None => invoke,
        };
        let adapter_result = if target.ret == Ty::Unit {
            Ty::Unit
        } else {
            erased_result
        };
        let body = self
            .ir
            .callable_reference_adapter_body(invoke, erased_result, adapter_result);
        let mut parameters = Vec::with_capacity(target.params.len() + 1);
        parameters.push(from);
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
        self.ir
            .set_method_visibility(adapter, crate::types::Visibility::Private);
        // The converted value is the adapter's bound receiver, ahead of its own parameters.
        self.ir.lambda_own_params_from.insert(adapter, 1);
        self.attach_generated_static_to_lexical_class(adapter);
        self.ir
            .add_expr(IrExpr::CallableReference(Box::new(IrCallableReference {
                target: IrCallableReferenceTarget::FunctionValueConversion { ordinal },
                adapter,
                captures: Vec::new(),
                bound_receiver: Some(value),
                bound_contexts: 0,
                function_type: to,
                declaration_parameters: parameters.into_boxed_slice(),
                declaration_result: target.ret,
                declaration_suspend: target.suspend,
                adaptation: None,
                reflection_owner: None,
            })))
    }

    /// `value?.invoke(...)` over the temporary `variable` declares, yielding `result`: `null` when
    /// the value is `null`, else the invocation's result.
    fn null_guarded_invoke(
        &mut self,
        variable: ExprId,
        temporary: u32,
        invoke: ExprId,
        result: Ty,
    ) -> ExprId {
        let read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let null = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let condition =
            self.ir
                .add_expr(IrExpr::primitive_bin_op(crate::ir::IrBinOp::Eq, read, null));
        let null_result = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let guarded = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), null_result), (None, invoke)],
        });
        self.ir.whens.exhaustive.insert(guarded, result);
        self.ir.set_logical_type(guarded, result);
        self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable],
            value: Some(guarded),
        })
    }
}
