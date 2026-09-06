use crate::fir::{
    ControlTargetId, FirBuiltinIterableKind, FirExprId, FirIteratorCall, FirIteratorReceiver,
    FirLoopHeader, FirRangeCounterKind, FirRangeOperation, OriginId, ResolvedTy,
};
use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrIntrinsic, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    fn local_value_is_mutable(&self, value: crate::fir::LocalValueId) -> bool {
        (0..self.body.statement_count()).any(|raw| {
            let statement = self
                .body
                .statement(crate::fir::FirStatementId::from_raw(raw as u32))
                .expect("FIR statement index");
            match &statement.kind {
                crate::fir::FirStatementKind::Local {
                    target, mutable, ..
                } => *target == value && *mutable,
                crate::fir::FirStatementKind::Destructure { entries, .. } => {
                    entries.iter().any(|entry| {
                        matches!(
                            entry,
                            crate::fir::FirDestructureEntry::Binding {
                                target,
                                mutable: true,
                                ..
                            } if *target == value
                        )
                    })
                }
                _ => false,
            }
        })
    }

    fn loop_variable_declaration(
        &mut self,
        variable: u32,
        ty: Ty,
        initializer: ExprId,
    ) -> (u32, ExprId) {
        let source = crate::fir::LocalValueId::from_raw(variable);
        let variable = self.value_slot(source);
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: variable,
            ty,
            init: Some(initializer),
            named: true,
        });
        if let Some(name) = self.body.debug_value_name(source) {
            self.ir.value_names.insert(declaration, name.to_owned());
        }
        (variable, declaration)
    }

    pub(super) fn loop_statement(
        &mut self,
        target: ControlTargetId,
        header: &FirLoopHeader,
        body: FirExprId,
        _origin: OriginId,
    ) -> Result<ExprId, FirLoweringFailure> {
        match header {
            FirLoopHeader::While { condition } => self.while_loop(target, *condition, body, false),
            FirLoopHeader::DoWhile { condition } => self.while_loop(target, *condition, body, true),
            FirLoopHeader::Range {
                variable,
                counter,
                operation,
                start,
                end,
            } => self.range_loop(
                target,
                variable.raw(),
                *counter,
                *operation,
                *start,
                *end,
                body,
            ),
            FirLoopHeader::Iterable {
                variable,
                variable_ty,
                kind,
                iterable,
            } => self.iterable_loop(target, variable.raw(), *variable_ty, *kind, *iterable, body),
            FirLoopHeader::Iterator {
                variable,
                variable_ty,
                iterable,
                iterator_ty,
                iterator,
                has_next,
                next,
            } => self.iterator_loop(
                target,
                variable.raw(),
                *variable_ty,
                *iterable,
                *iterator_ty,
                iterator,
                has_next,
                next,
                body,
            ),
        }
    }

    fn while_loop(
        &mut self,
        target: ControlTargetId,
        condition: FirExprId,
        body: FirExprId,
        post_test: bool,
    ) -> Result<ExprId, FirLoweringFailure> {
        let condition = self.expression(condition)?;
        let body = self.expression(body)?;
        Ok(self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: None,
            post_test,
            label: Some(self.control_label(0, target)?),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn range_loop(
        &mut self,
        target: ControlTargetId,
        variable: u32,
        counter: FirRangeCounterKind,
        operation: FirRangeOperation,
        start: FirExprId,
        end: FirExprId,
        body: FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let ty = counter.ty();
        if matches!(ty, Ty::UInt | Ty::ULong) {
            let start = self.expression(start)?;
            let end = self.expression(end)?;
            let body = self.expression(body)?;
            return Ok(self.ir.add_expr(IrExpr::Checked(
                crate::ir::IrCheckedOperation::RangeLoop {
                    variable: self.value_slot(crate::fir::LocalValueId::from_raw(variable)),
                    counter: ty,
                    operation,
                    start,
                    end,
                    body,
                    label: self.control_label(0, target)?,
                },
            )));
        }
        let start = self.expression(start)?;
        let start = self.coerce(start, ty);
        let (variable, variable_declaration) = self.loop_variable_declaration(variable, ty, start);
        let end_value = self.expression(end)?;
        let end_value = self.coerce(end_value, ty);
        let constant_end = matches!(
            self.ir.expr(end_value),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if matches!(self.ir.expr(*arg), IrExpr::Const(_))
        );
        let (end_declaration, end_read) = if constant_end {
            (None, end_value)
        } else {
            let end_slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: end_slot,
                ty,
                init: Some(end_value),
                named: false,
            });
            let read = self.ir.add_expr(IrExpr::GetValue(end_slot));
            (Some(declaration), read)
        };
        let counter_read = self.ir.add_expr(IrExpr::GetValue(variable));
        let comparison = match operation {
            FirRangeOperation::Through => IrBinOp::Le,
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => IrBinOp::Lt,
            FirRangeOperation::DownTo => IrBinOp::Ge,
        };
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: comparison,
            lhs: counter_read,
            rhs: end_read,
        });
        let body = self.expression(body)?;
        let counter_read = self.ir.add_expr(IrExpr::GetValue(variable));
        let step = self.ir.add_expr(IrExpr::Const(if ty == Ty::Long {
            IrConst::Long(1)
        } else {
            IrConst::Int(1)
        }));
        let updated = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: if operation == FirRangeOperation::DownTo {
                IrBinOp::Sub
            } else {
                IrBinOp::Add
            },
            lhs: counter_read,
            rhs: step,
        });
        let updated = if ty == Ty::Char {
            self.coerce(updated, ty)
        } else {
            updated
        };
        let write = self.ir.add_expr(IrExpr::SetValue {
            var: variable,
            value: updated,
        });
        let update = if matches!(
            operation,
            FirRangeOperation::OpenEnd | FirRangeOperation::Until
        ) {
            write
        } else {
            let counter_read = self.ir.add_expr(IrExpr::GetValue(variable));
            let at_end = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: counter_read,
                rhs: end_read,
            });
            let break_expression = self.ir.add_expr(IrExpr::Break {
                label: Some(self.control_label(0, target)?),
            });
            let guard = self.ir.add_expr(IrExpr::When {
                branches: vec![(Some(at_end), break_expression)],
            });
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![guard, write],
                value: None,
            })
        };
        let loop_expression = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: Some(update),
            post_test: false,
            label: Some(self.control_label(0, target)?),
        });
        let mut statements = vec![variable_declaration];
        statements.extend(end_declaration);
        statements.push(loop_expression);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn iterable_loop(
        &mut self,
        target: ControlTargetId,
        variable: u32,
        variable_ty: ResolvedTy,
        kind: FirBuiltinIterableKind,
        iterable: FirExprId,
        body: FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let iterable_expression = self
            .body
            .expr(iterable)
            .ok_or(FirLoweringFailure::MissingExpression(iterable))?;
        let iterable_ty = iterable_expression.ty.get().non_null();
        let stable_local = match &iterable_expression.kind {
            crate::fir::FirExprKind::ValueRead(value) if !self.local_value_is_mutable(*value) => {
                Some(*value)
            }
            _ => None,
        };
        let (size_operation, get_operation) = match kind {
            FirBuiltinIterableKind::Array => (IrIntrinsic::ArraySize, IrIntrinsic::ArrayGet),
            FirBuiltinIterableKind::String => (IrIntrinsic::StringLength, IrIntrinsic::StringGet),
        };
        let iterable_value = self.expression(iterable)?;
        let stable_slot = stable_local.and_then(|value| {
            matches!(self.ir.expr(iterable_value), IrExpr::GetValue(slot) if *slot == self.value_slot(value))
                .then_some(self.value_slot(value))
        });
        let (iterable_slot, iterable_declaration) = if let Some(slot) = stable_slot {
            (slot, None)
        } else {
            let slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: iterable_ty,
                init: Some(iterable_value),
                named: false,
            });
            (slot, Some(declaration))
        };
        let index_slot = self.allocate_temporary();
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let index_declaration = self.ir.add_expr(IrExpr::Variable {
            index: index_slot,
            ty: Ty::Int,
            init: Some(zero),
            named: false,
        });
        let receiver = self.ir.add_expr(IrExpr::GetValue(iterable_slot));
        let size = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: size_operation,
                ret: Ty::Int,
            },
            dispatch_receiver: Some(receiver),
            args: Vec::new(),
        });
        let size_slot = self.allocate_temporary();
        let size_declaration = self.ir.add_expr(IrExpr::Variable {
            index: size_slot,
            ty: Ty::Int,
            init: Some(size),
            named: false,
        });
        let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let size_read = self.ir.add_expr(IrExpr::GetValue(size_slot));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: index_read,
            rhs: size_read,
        });
        let receiver = self.ir.add_expr(IrExpr::GetValue(iterable_slot));
        let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let element = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: get_operation,
                ret: variable_ty.get(),
            },
            dispatch_receiver: Some(receiver),
            args: vec![index_read],
        });
        let (_, variable_declaration) =
            self.loop_variable_declaration(variable, variable_ty.get(), element);
        let body = self.expression(body)?;
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable_declaration, body],
            value: None,
        });
        let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
        let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let next_index = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: index_read,
            rhs: one,
        });
        let update = self.ir.add_expr(IrExpr::SetValue {
            var: index_slot,
            value: next_index,
        });
        let loop_expression = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: Some(update),
            post_test: false,
            label: Some(self.control_label(0, target)?),
        });
        let mut statements = Vec::with_capacity(4);
        statements.extend(iterable_declaration);
        statements.extend([index_declaration, size_declaration, loop_expression]);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn iterator_loop(
        &mut self,
        target: ControlTargetId,
        variable: u32,
        variable_ty: ResolvedTy,
        iterable: FirExprId,
        iterator_ty: ResolvedTy,
        iterator: &FirIteratorCall,
        has_next: &FirIteratorCall,
        next: &FirIteratorCall,
        body: FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let iterable_ty = self
            .body
            .expr(iterable)
            .ok_or(FirLoweringFailure::MissingExpression(iterable))?
            .ty
            .get();
        let iterable_value = self.expression(iterable)?;
        let iterable_slot = self.allocate_temporary();
        let iterable_declaration = self.ir.add_expr(IrExpr::Variable {
            index: iterable_slot,
            ty: iterable_ty,
            init: Some(iterable_value),
            named: false,
        });

        let iterable_read = self.ir.add_expr(IrExpr::GetValue(iterable_slot));
        let iterator_value = self.iterator_call(iterator, iterable_read)?;
        let iterator_slot = self.allocate_temporary();
        let iterator_declaration = self.ir.add_expr(IrExpr::Variable {
            index: iterator_slot,
            ty: iterator_ty.get(),
            init: Some(iterator_value),
            named: false,
        });

        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let condition = self.iterator_call(has_next, iterator_read)?;
        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let element = self.iterator_call(next, iterator_read)?;
        let (_, variable_declaration) =
            self.loop_variable_declaration(variable, variable_ty.get(), element);
        let body = self.expression(body)?;
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable_declaration, body],
            value: None,
        });
        let loop_expression = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: None,
            post_test: false,
            label: Some(self.control_label(0, target)?),
        });
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![iterable_declaration, iterator_declaration, loop_expression],
            value: None,
        }))
    }

    pub(super) fn iterator_call(
        &mut self,
        call: &FirIteratorCall,
        receiver: ExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let context_parameter_types = call
            .context_arguments
            .iter()
            .map(|argument| argument.parameter_type.get())
            .collect::<Vec<_>>();
        let arguments = call
            .context_arguments
            .iter()
            .enumerate()
            .map(|(parameter, argument)| {
                Ok(crate::ir::IrCheckedArgument::Expression {
                    parameter: u32::try_from(parameter)
                        .map_err(|_| FirLoweringFailure::UnsupportedIntrinsicCall)?,
                    value: self.expression_with_conversion(
                        argument.receiver.value,
                        argument.receiver.conversion,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
        let (dispatch_receiver, extension_receiver) = match &call.receiver {
            FirIteratorReceiver::Dispatch => (Some(receiver), None),
            FirIteratorReceiver::Extension => (None, Some(receiver)),
            FirIteratorReceiver::MemberExtension { dispatch_receiver } => (
                Some(self.expression_with_conversion(
                    dispatch_receiver.value,
                    dispatch_receiver.conversion,
                )?),
                Some(receiver),
            ),
        };
        match &call.target {
            // A loop protocol is never reached through `super`.
            crate::fir::FirCallTarget::Super { .. } => {
                Err(FirLoweringFailure::UnsupportedIntrinsicCall)
            }
            crate::fir::FirCallTarget::Classifier { .. } => {
                Err(FirLoweringFailure::UnsupportedIntrinsicCall)
            }
            crate::fir::FirCallTarget::Module(target) => self
                .same_file_call(
                    *target,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                    &context_parameter_types,
                    &[],
                )
                .ok_or(FirLoweringFailure::MissingCallable(*target)),
            crate::fir::FirCallTarget::External {
                declaration,
                default_provider,
                receiver,
                declared_receiver,
                parameters,
                result,
                declared_result,
                suspend,
                can_inline,
                inline_plan,
                extension_receiver_parameter,
            } => self
                .external_call(super::source_calls::ExternalCallRequest {
                    target: *declaration,
                    default_provider: *default_provider,
                    receiver_ty: *receiver,
                    declared_receiver: *declared_receiver,
                    parameters,
                    result: *result,
                    declared_result: *declared_result,
                    suspend: *suspend,
                    can_inline: *can_inline,
                    inline_plan: inline_plan.as_deref(),
                    substitutions: &[],
                    extension_receiver_parameter: *extension_receiver_parameter,
                    dispatch_receiver,
                    extension_receiver,
                    arguments: &arguments,
                })
                .ok_or(FirLoweringFailure::UnsupportedExternalCall(*declaration)),
            crate::fir::FirCallTarget::Intrinsic {
                operation,
                receiver,
                parameters,
                result,
            } => self
                .intrinsic_call(
                    super::checked::lower_fir_intrinsic(operation),
                    *receiver,
                    parameters,
                    *result,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                )
                .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall),
        }
    }

    fn coerce(&mut self, expression: ExprId, target: Ty) -> ExprId {
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: expression,
            type_operand: target,
        })
    }
}
