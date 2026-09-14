//! Lowering of complete checked inline-body contracts.

use crate::fir::ResolvedTy;
use crate::ir::{Callee, ExprId, IrCheckedArgument, IrConst, IrExpr};
use crate::types::Ty;

use super::source_calls::{
    rehome_inline_body_values, SelectedDefaultMode, SelectedOperandMode, SelectedOperandRequest,
};
use super::BodyLowering;

pub(super) struct ExternalInlineCallRequest<'a> {
    pub(super) plan: &'a crate::fir::FirInlineBodyPlan,
    pub(super) receiver_ty: Option<ResolvedTy>,
    pub(super) parameter_types: &'a [Ty],
    pub(super) result: ResolvedTy,
    pub(super) dispatch_receiver: Option<ExprId>,
    pub(super) extension_receiver: Option<ExprId>,
    pub(super) arguments: &'a [IrCheckedArgument],
}

impl BodyLowering<'_> {
    pub(super) fn external_inline_call(
        &mut self,
        request: ExternalInlineCallRequest<'_>,
    ) -> Option<ExprId> {
        let ExternalInlineCallRequest {
            plan,
            receiver_ty,
            parameter_types,
            result,
            dispatch_receiver,
            extension_receiver,
            arguments,
        } = request;
        let (
            lambda_parameter,
            invocation_arguments,
            prologue,
            cleanup,
            cause_ty,
            plan_defaults,
            returned_value,
        ) = match plan {
            crate::fir::FirInlineBodyPlan::ForEach {
                lambda_parameter,
                iterator_ty,
                iterator,
                has_next,
                next,
            } => {
                return self.external_inline_for_each(
                    *lambda_parameter,
                    *iterator_ty,
                    iterator,
                    has_next,
                    next,
                    receiver_ty,
                    parameter_types,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::CollectionTransform {
                lambda_parameter,
                flatten,
                local_names,
                iterator_ty,
                iterator,
                has_next,
                next,
                factory,
                factory_classifier,
                append,
                accumulator,
                append_parameter,
                append_result,
            } => {
                return self.external_inline_collection_transform(
                    *lambda_parameter,
                    *flatten,
                    local_names,
                    *iterator_ty,
                    iterator,
                    has_next,
                    next,
                    *factory,
                    *factory_classifier,
                    *append,
                    *accumulator,
                    *append_parameter,
                    *append_result,
                    receiver_ty,
                    parameter_types,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::InvokeLambda {
                lambda_parameter,
                arguments,
                prologue,
                cleanup,
                cause,
                defaults,
                result: returned,
            } => (
                *lambda_parameter,
                arguments,
                prologue,
                cleanup,
                *cause,
                defaults,
                *returned,
            ),
        };
        let lambda_parameter = lambda_parameter as usize;
        let (mut statements, receiver, mut args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Materialize,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::Materialized,
            })?;
        for omitted in &defaults {
            let default = plan_defaults
                .iter()
                .find(|default| default.parameter == *omitted)?;
            let value = match default.value {
                crate::fir::FirInlineDefaultValue::Null => {
                    self.ir.add_expr(IrExpr::Const(IrConst::Null))
                }
            };
            *args.get_mut(*omitted as usize)? = value;
        }

        let cause = match cause_ty {
            None => None,
            Some(cause_ty) => {
                let cause_ty = cause_ty.get();
                let slot = self.allocate_temporary();
                let initial = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                statements.push(self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: cause_ty,
                    init: Some(initial),
                    named: false,
                }));
                Some((slot, cause_ty))
            }
        };
        let plan_value = |lowering: &mut Self, value: crate::fir::FirInlineValue| match value {
            crate::fir::FirInlineValue::Receiver => Some((receiver?, receiver_ty?.get())),
            crate::fir::FirInlineValue::Parameter(parameter) => Some((
                *args.get(parameter as usize)?,
                *parameter_types.get(parameter as usize)?,
            )),
            crate::fir::FirInlineValue::Cause => {
                let (slot, ty) = cause?;
                Some((lowering.ir.add_expr(IrExpr::GetValue(slot)), ty))
            }
        };
        for call in prologue {
            statements.push(self.external_inline_plan_call(call, plan_value)?);
        }
        let invocation_operands = invocation_arguments
            .iter()
            .map(|operand| plan_value(self, *operand))
            .collect::<Option<Vec<_>>>()?;
        let inline_body = self.materialize_external_inline_lambda(
            &mut statements,
            &args,
            lambda_parameter,
            &invocation_operands,
        )?;

        let value = if cleanup.is_empty() {
            inline_body
        } else {
            self.guard_inline_body(inline_body, cleanup, cause, result.get(), plan_value)?
        };
        if let Some(returned_value) = returned_value {
            statements.push(value);
            let (returned, _) = plan_value(self, returned_value)?;
            return Some(self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(returned),
            }));
        }
        Some(if statements.is_empty() {
            value
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(value),
            })
        })
    }

    fn guard_inline_body(
        &mut self,
        body: ExprId,
        cleanup: &[crate::fir::FirInlineCall],
        cause: Option<(u32, Ty)>,
        result_ty: Ty,
        plan_value: impl Fn(&mut Self, crate::fir::FirInlineValue) -> Option<(ExprId, Ty)> + Copy,
    ) -> Option<ExprId> {
        let result_slot = self.allocate_temporary();
        let initial = self
            .ir
            .add_expr(IrExpr::Const(IrConst::zero_for_value_type(result_ty)));
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: result_slot,
            ty: result_ty,
            init: Some(initial),
            named: false,
        });
        let store_result = self.ir.add_expr(IrExpr::SetValue {
            var: result_slot,
            value: body,
        });
        let try_body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![store_result],
            value: None,
        });
        let catches = match cause {
            None => Vec::new(),
            Some((cause_slot, cause_ty)) => {
                let caught = self.allocate_temporary();
                let caught_value = self.ir.add_expr(IrExpr::GetValue(caught));
                let record = self.ir.add_expr(IrExpr::SetValue {
                    var: cause_slot,
                    value: caught_value,
                });
                let rethrown = self.ir.add_expr(IrExpr::GetValue(caught));
                let rethrow = self.ir.add_expr(IrExpr::Throw { operand: rethrown });
                vec![crate::ir::IrCatch {
                    var: caught,
                    name: None,
                    exc_internal: cause_ty.non_null().obj_internal()?,
                    body: self.ir.add_expr(IrExpr::Block {
                        stmts: vec![record, rethrow],
                        value: None,
                    }),
                }]
            }
        };
        let cleanup_calls = cleanup
            .iter()
            .map(|call| self.external_inline_plan_call(call, plan_value))
            .collect::<Option<Vec<_>>>()?;
        let finally = self.ir.add_expr(IrExpr::Block {
            stmts: cleanup_calls,
            value: None,
        });
        let guarded = self.ir.add_expr(IrExpr::Try {
            body: try_body,
            catches,
            finally: Some(finally),
            result: Ty::Unit,
        });
        let value = self.ir.add_expr(IrExpr::GetValue(result_slot));
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, guarded],
            value: Some(value),
        }))
    }

    fn external_inline_plan_call(
        &mut self,
        call: &crate::fir::FirInlineCall,
        plan_value: impl Fn(&mut Self, crate::fir::FirInlineValue) -> Option<(ExprId, Ty)>,
    ) -> Option<ExprId> {
        let receiver = match call.receiver {
            None => None,
            Some(crate::fir::FirInlineCallReceiver::Dispatch(value)) => {
                Some(plan_value(self, value)?)
            }
            Some(crate::fir::FirInlineCallReceiver::Extension(value)) => {
                Some(plan_value(self, value)?)
            }
        };
        let args = call
            .arguments
            .iter()
            .map(|value| Some(plan_value(self, *value)?.0))
            .collect::<Option<Vec<_>>>()?;
        if call.parameters.len() != args.len() {
            return None;
        }
        let expression = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External {
                target: call.declaration,
                default_provider: None,
                params: call
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                ret: call.result.get(),
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            },
            dispatch_receiver: receiver.map(|(receiver, _)| receiver),
            args,
        });
        if let Some((_, receiver_ty)) = receiver {
            self.ir
                .ext_call_source_receiver
                .insert(expression, receiver_ty);
        }
        if call.suspend {
            self.ir.suspend_calls.insert(expression, call.result.get());
        }
        Some(expression)
    }

    /// Replace one already-evaluated lambda operand with its checked inline-body template. Capture
    /// evaluation stays at the lambda's source position, and every external inline shape shares this
    /// single local-slot rebasing path.
    fn materialize_external_inline_lambda(
        &mut self,
        statements: &mut Vec<ExprId>,
        args: &[ExprId],
        lambda_parameter: usize,
        invocation_operands: &[(ExprId, Ty)],
    ) -> Option<ExprId> {
        let lambda_slot = match self.ir.expr(*args.get(lambda_parameter)?) {
            IrExpr::GetValue(slot) => *slot,
            _ => return None,
        };
        let declaration_position = statements.iter().position(|statement| {
            matches!(
                self.ir.expr(*statement),
                IrExpr::Variable { index, .. } if *index == lambda_slot
            )
        })?;
        let lambda = match self.ir.expr(statements[declaration_position]).clone() {
            IrExpr::Variable {
                init: Some(lambda), ..
            } => lambda,
            _ => return None,
        };
        let (implementation, captures, inline_body, arity) = match self.ir.expr(lambda).clone() {
            IrExpr::Lambda {
                impl_fn,
                captures,
                inline_body: Some(inline_body),
                arity,
                ..
            } => (impl_fn, captures, inline_body, arity as usize),
            _ => return None,
        };
        if invocation_operands.len() != arity {
            return None;
        }

        statements.remove(declaration_position);
        let mut capture_declarations = Vec::with_capacity(captures.len());
        let mut formal_slots = Vec::with_capacity(captures.len() + arity);
        for (capture_ordinal, capture) in captures.into_iter().enumerate() {
            if self.ir.shared_capture_parameters.contains_key(&(
                implementation,
                u32::try_from(capture_ordinal).expect("too many inline captures"),
            )) {
                let IrExpr::GetValue(slot) = self.ir.expr(capture) else {
                    return None;
                };
                formal_slots.push(*slot);
                continue;
            }
            let slot = self.allocate_temporary();
            let ty = *self
                .ir
                .functions
                .get(implementation as usize)?
                .params
                .get(capture_ordinal)?;
            capture_declarations.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(capture),
                named: false,
            }));
            formal_slots.push(slot);
        }
        statements.splice(
            declaration_position..declaration_position,
            capture_declarations,
        );
        for &(value, ty) in invocation_operands {
            let slot = match self.ir.expr(value) {
                IrExpr::GetValue(slot) => *slot,
                _ => {
                    let slot = self.allocate_temporary();
                    statements.push(self.ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty,
                        init: Some(value),
                        named: false,
                    }));
                    slot
                }
            };
            formal_slots.push(slot);
        }

        let local_base = self.next_temporary;
        let local_count =
            rehome_inline_body_values(self.ir, inline_body, &formal_slots, local_base)?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.functions[implementation as usize].body = None;
        self.ir.inline_only_fns.insert(implementation);
        Some(inline_body)
    }
}
