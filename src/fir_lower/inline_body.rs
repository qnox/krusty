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
            recovery,
            plan_defaults,
            returned_value,
        ) = match plan {
            crate::fir::FirInlineBodyPlan::Iteration {
                lambda_parameter,
                element,
                index,
                traversal,
            } => {
                return self.external_inline_iteration(
                    *lambda_parameter,
                    *element,
                    index.as_ref(),
                    traversal,
                    receiver_ty,
                    parameter_types,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::CollectionTransform {
                lambda_parameter,
                local_names,
                traversal,
                factory,
                factory_classifier,
                factory_parameters,
                capacity,
                append,
                accumulator,
            } => {
                return self.external_inline_collection_transform(
                    *lambda_parameter,
                    local_names,
                    traversal,
                    *factory,
                    *factory_classifier,
                    factory_parameters,
                    capacity.as_ref(),
                    append,
                    *accumulator,
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
                recovery,
                defaults,
                result: returned,
            } => (
                *lambda_parameter,
                arguments,
                prologue,
                cleanup,
                *cause,
                recovery.as_deref(),
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

        let value = if let Some(recovery) = recovery {
            if !cleanup.is_empty() || cause.is_some() {
                return None;
            }
            self.recover_inline_body(inline_body, recovery, result.get())?
        } else if cleanup.is_empty() {
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

    fn recover_inline_body(
        &mut self,
        body: ExprId,
        recovery: &crate::fir::FirInlineRecovery,
        result_ty: Ty,
    ) -> Option<ExprId> {
        let [constructor_parameter] = recovery.constructor_parameters.as_ref() else {
            return None;
        };
        let construct = |lowering: &mut Self, value| {
            lowering.ir.add_expr(IrExpr::New {
                internal: recovery.classifier,
                args: vec![value],
                ctor_params: Some(vec![constructor_parameter.get()]),
                ctor_desc: None,
                external_target: Some(recovery.constructor),
                defaults: Box::new([]),
                default_prefix_count: 0,
            })
        };
        let normal = construct(self, body);
        let caught_ty = recovery.caught.get().non_null();
        let caught_internal = caught_ty.obj_internal()?;
        let caught = self.allocate_temporary();
        let failure =
            self.external_inline_plan_call(&recovery.failure, |lowering, value| match value {
                crate::fir::FirInlineValue::Cause => {
                    Some((lowering.ir.add_expr(IrExpr::GetValue(caught)), caught_ty))
                }
                crate::fir::FirInlineValue::Receiver | crate::fir::FirInlineValue::Parameter(_) => {
                    None
                }
            })?;
        let failed = construct(self, failure);
        Some(self.ir.add_expr(IrExpr::Try {
            body: normal,
            catches: vec![crate::ir::IrCatch {
                var: caught,
                name: None,
                exc_internal: caught_internal,
                body: failed,
            }],
            finally: None,
            result: result_ty,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn external_inline_iteration(
        &mut self,
        lambda_parameter: u32,
        element_ty: ResolvedTy,
        index: Option<&crate::fir::FirInlineIterationIndex>,
        traversal: &crate::fir::FirInlineIterationTraversal,
        receiver_ty: Option<ResolvedTy>,
        parameter_types: &[Ty],
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let (mut statements, receiver, args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Reject,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::Materialized,
            })?;
        debug_assert!(defaults.is_empty());
        let iterable = receiver?;
        let lambda_slot = match self.ir.expr(*args.get(lambda_parameter as usize)?) {
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
        if arity != 1 + usize::from(index.is_some()) {
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

        let action_index_slot = index.map(|_| self.allocate_temporary());
        let current_index_slot = index.map(|_| self.allocate_temporary());
        if let Some(current_index_slot) = current_index_slot {
            formal_slots.push(current_index_slot);
        }
        let element_slot = self.allocate_temporary();
        formal_slots.push(element_slot);
        let local_base = self.next_temporary;
        let local_count =
            rehome_inline_body_values(self.ir, inline_body, &formal_slots, local_base)?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.functions[implementation as usize].body = None;
        self.ir.inline_only_fns.insert(implementation);

        if let Some(action_index_slot) = action_index_slot {
            let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            statements.push(self.ir.add_expr(IrExpr::Variable {
                index: action_index_slot,
                ty: Ty::Int,
                init: Some(zero),
                named: true,
            }));
        }
        let (condition, element, update, loop_identity) = match traversal {
            crate::fir::FirInlineIterationTraversal::Iterator {
                prepare,
                has_next,
                next,
            } => {
                let mut current = iterable;
                for call in prepare {
                    current = self.external_inline_iteration_member_call(call, current, &[])?;
                }
                let iterator_ty = prepare.last()?.result.get();
                let iterator_slot = self.allocate_temporary();
                statements.push(self.ir.add_expr(IrExpr::Variable {
                    index: iterator_slot,
                    ty: iterator_ty,
                    init: Some(current),
                    named: false,
                }));
                let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
                let condition =
                    self.external_inline_iteration_member_call(has_next, iterator_read, &[])?;
                let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
                let element =
                    self.external_inline_iteration_member_call(next, iterator_read, &[])?;
                (condition, element, None, iterator_slot)
            }
            crate::fir::FirInlineIterationTraversal::Array
            | crate::fir::FirInlineIterationTraversal::Counted { .. } => {
                let receiver_slot = self.allocate_temporary();
                statements.push(self.ir.add_expr(IrExpr::Variable {
                    index: receiver_slot,
                    ty: receiver_ty?.get(),
                    init: Some(iterable),
                    named: false,
                }));
                let receiver = self.ir.add_expr(IrExpr::GetValue(receiver_slot));
                let size = match traversal {
                    crate::fir::FirInlineIterationTraversal::Array => {
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Intrinsic {
                                operation: crate::ir::IrIntrinsic::ArraySize,
                                ret: Ty::Int,
                            },
                            dispatch_receiver: Some(receiver),
                            args: Vec::new(),
                        })
                    }
                    crate::fir::FirInlineIterationTraversal::Counted { size, .. } => {
                        self.external_inline_iteration_member_call(size, receiver, &[])?
                    }
                    crate::fir::FirInlineIterationTraversal::Iterator { .. } => unreachable!(),
                };
                let size = if matches!(traversal, crate::fir::FirInlineIterationTraversal::Array) {
                    let size_slot = self.allocate_temporary();
                    statements.push(self.ir.add_expr(IrExpr::Variable {
                        index: size_slot,
                        ty: Ty::Int,
                        init: Some(size),
                        named: false,
                    }));
                    self.ir.add_expr(IrExpr::GetValue(size_slot))
                } else {
                    size
                };
                let cursor_slot = self.allocate_temporary();
                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                statements.push(self.ir.add_expr(IrExpr::Variable {
                    index: cursor_slot,
                    ty: Ty::Int,
                    init: Some(zero),
                    named: false,
                }));
                let cursor = self.ir.add_expr(IrExpr::GetValue(cursor_slot));
                let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Lt,
                    lhs: cursor,
                    rhs: size,
                });
                let receiver = self.ir.add_expr(IrExpr::GetValue(receiver_slot));
                let cursor = self.ir.add_expr(IrExpr::GetValue(cursor_slot));
                let element = match traversal {
                    crate::fir::FirInlineIterationTraversal::Array => {
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Intrinsic {
                                operation: crate::ir::IrIntrinsic::ArrayGet,
                                ret: element_ty.get(),
                            },
                            dispatch_receiver: Some(receiver),
                            args: vec![cursor],
                        })
                    }
                    crate::fir::FirInlineIterationTraversal::Counted { get, .. } => self
                        .external_inline_iteration_member_call(
                            get,
                            receiver,
                            &[(cursor, Ty::Int)],
                        )?,
                    crate::fir::FirInlineIterationTraversal::Iterator { .. } => unreachable!(),
                };
                let cursor = self.ir.add_expr(IrExpr::GetValue(cursor_slot));
                let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                let incremented = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Add,
                    lhs: cursor,
                    rhs: one,
                });
                let update = self.ir.add_expr(IrExpr::SetValue {
                    var: cursor_slot,
                    value: incremented,
                });
                (condition, element, Some(update), cursor_slot)
            }
        };
        let loop_label = format!("$fir_inline_iteration_{loop_identity}");
        let element_declaration = self.ir.add_expr(IrExpr::Variable {
            index: element_slot,
            ty: element_ty.get(),
            init: Some(element),
            named: true,
        });
        let mut body_statements = vec![element_declaration];
        if let (Some(action_index_slot), Some(current_index_slot), Some(index)) =
            (action_index_slot, current_index_slot, index)
        {
            let current = self.ir.add_expr(IrExpr::GetValue(action_index_slot));
            body_statements.push(self.ir.add_expr(IrExpr::Variable {
                index: current_index_slot,
                ty: Ty::Int,
                init: Some(current),
                named: true,
            }));
            let current = self.ir.add_expr(IrExpr::GetValue(current_index_slot));
            let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
            let incremented = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Add,
                lhs: current,
                rhs: one,
            });
            body_statements.push(self.ir.add_expr(IrExpr::SetValue {
                var: action_index_slot,
                value: incremented,
            }));
            if let crate::fir::FirInlineIterationIndex::Checked { overflow } = index {
                let current = self.ir.add_expr(IrExpr::GetValue(current_index_slot));
                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                let negative = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Lt,
                    lhs: current,
                    rhs: zero,
                });
                let overflow = self.external_inline_plan_call(overflow, |_, _| None)?;
                body_statements.push(self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(negative), overflow)],
                }));
            }
        }
        body_statements.push(inline_body);
        let loop_body = self.ir.add_expr(IrExpr::Block {
            stmts: body_statements,
            value: None,
        });
        statements.push(self.ir.add_expr(IrExpr::While {
            cond: condition,
            body: loop_body,
            update,
            post_test: false,
            label: Some(loop_label),
        }));
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(unit),
        }))
    }

    pub(super) fn external_inline_iteration_member_call(
        &mut self,
        call: &crate::fir::FirInlineIterationMemberCall,
        receiver: ExprId,
        arguments: &[(ExprId, Ty)],
    ) -> Option<ExprId> {
        if arguments.len() != call.parameters.len()
            || arguments
                .iter()
                .zip(&call.parameters)
                .any(|((_, actual), expected)| *actual != expected.get())
        {
            return None;
        }
        let arguments = arguments
            .iter()
            .enumerate()
            .map(|(parameter, (value, _))| {
                Some(IrCheckedArgument::Expression {
                    parameter: u32::try_from(parameter).ok()?,
                    value: *value,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        self.external_call(super::source_calls::ExternalCallRequest {
            target: call.declaration,
            default_provider: None,
            receiver_ty: Some(call.receiver),
            declared_receiver: None,
            parameters: &call.parameters,
            result: call.result,
            declared_result: None,
            suspend: false,
            can_inline: false,
            inline_plan: None,
            substitutions: &[],
            extension_receiver_parameter: None,
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            arguments: &arguments,
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
