use crate::fir::{
    ControlTargetId, FirBuiltinIterableKind, FirExprId, FirIteratorCall, FirIteratorReceiver,
    FirLoopHeader, FirProgressionClass, FirProgressionSource, FirRangeCounterKind, LocalValueId,
    OriginId, ResolvedTy,
};
use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrIntrinsic, IrProgressionSource};
use crate::types::Ty;

use super::source_calls::SameFileExtensionReceiverMode;
use super::{BodyLowering, FirLoweringFailure};

/// The checked pieces of one iterator-protocol loop, kept together across the FIR-to-IR boundary.
struct IteratorLoopContract<'a> {
    target: ControlTargetId,
    variable: LocalValueId,
    variable_ty: ResolvedTy,
    iterable: FirExprId,
    iterator_ty: ResolvedTy,
    iterator: &'a FirIteratorCall,
    has_next: &'a FirIteratorCall,
    next: &'a FirIteratorCall,
    body: FirExprId,
}

/// The checked semantic pieces of one counted loop over a progression. Common lowering preserves
/// this contract; each backend chooses its own counted-loop control-flow shape.
struct RangeLoopContract<'a> {
    target: ControlTargetId,
    variable: LocalValueId,
    counter: FirRangeCounterKind,
    source: std::borrow::Cow<'a, FirProgressionSource>,
    body: FirExprId,
}

impl BodyLowering<'_> {
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
            } => self.range_loop(RangeLoopContract {
                target,
                variable: *variable,
                counter: *counter,
                source: std::borrow::Cow::Owned(FirProgressionSource::Literal {
                    operation: *operation,
                    start: *start,
                    end: *end,
                }),
                body,
            }),
            FirLoopHeader::Progression {
                variable,
                counter,
                source,
            } => self.range_loop(RangeLoopContract {
                target,
                variable: *variable,
                counter: *counter,
                source: std::borrow::Cow::Borrowed(source),
                body,
            }),
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
            } => self.iterator_loop(IteratorLoopContract {
                target,
                variable: *variable,
                variable_ty: *variable_ty,
                iterable: *iterable,
                iterator_ty: *iterator_ty,
                iterator,
                has_next,
                next,
                body,
            }),
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

    fn range_loop(&mut self, lp: RangeLoopContract<'_>) -> Result<ExprId, FirLoweringFailure> {
        let source = self.progression_source(&lp.source)?;
        let body = self.expression(lp.body)?;
        Ok(self
            .ir
            .add_expr(IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop {
                variable: self.value_slot(lp.variable),
                variable_name: self.body.debug_value_name(lp.variable).map(Into::into),
                counter: lp.counter.ty(),
                source,
                body,
                label: self.control_label(0, lp.target)?,
            })))
    }

    /// Lowers the operands of a matched progression. A progression value gets the static class
    /// the checker built its header from (`irCastIfNeeded`).
    fn progression_source(
        &mut self,
        source: &FirProgressionSource,
    ) -> Result<IrProgressionSource, FirLoweringFailure> {
        Ok(match source {
            FirProgressionSource::Literal {
                operation,
                start,
                end,
            } => IrProgressionSource::Literal {
                operation: *operation,
                start: self.expression(*start)?,
                end: self.expression(*end)?,
            },
            FirProgressionSource::Value {
                progression,
                iterable,
            } => self.progression_value(progression, *iterable)?,
            FirProgressionSource::Step {
                nested,
                step,
                last_element,
            } => IrProgressionSource::Step {
                nested: Box::new(self.progression_source(nested)?),
                step: self.expression(*step)?,
                last_element: crate::ir::IrRuntimeFunction {
                    function: last_element.function,
                    parameters: last_element.parameters.to_vec(),
                    result: last_element.result,
                },
            },
            FirProgressionSource::Reversed(nested) => {
                IrProgressionSource::Reversed(Box::new(self.progression_source(nested)?))
            }
        })
    }

    /// `DefaultProgressionHandler`: the progression is read once, into a temporary unless it is a
    /// constant or a local read, and the loop reads the selected `first`, `last` and `step` from
    /// it.
    fn progression_value(
        &mut self,
        progression: &FirProgressionClass,
        iterable: FirExprId,
    ) -> Result<IrProgressionSource, FirLoweringFailure> {
        let value = self.expression(iterable)?;
        let iterable_ty = self
            .body
            .expr(iterable)
            .map(|expression| expression.ty.get());
        let value = if iterable_ty == Some(progression.ty) {
            value
        } else {
            self.ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: value,
                type_operand: progression.ty,
            })
        };
        let (setup, value) =
            if matches!(self.ir.expr(value), IrExpr::GetValue(_) | IrExpr::Const(_)) {
                (None, value)
            } else {
                let slot = self.allocate_temporary();
                let setup = self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: progression.ty,
                    init: Some(value),
                    named: false,
                });
                (Some(setup), self.ir.add_expr(IrExpr::GetValue(slot)))
            };
        let first = self.progression_member_read(&progression.first, value)?;
        let last = self.progression_member_read(&progression.last, value)?;
        let step = progression
            .step
            .as_ref()
            .map(|step| self.progression_member_read(step, value))
            .transpose()?;
        Ok(IrProgressionSource::Value {
            setup,
            first,
            last,
            step,
        })
    }

    /// A read of a selected progression member on the stored progression `value` (a leaf read,
    /// re-read for each member).
    fn progression_member_read(
        &mut self,
        target: &crate::fir::FirPropertyTarget,
        value: ExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let crate::fir::FirPropertyTarget::External {
            property,
            receiver,
            parameters,
            result,
            extension_receiver_parameter,
            dispatch,
        } = target
        else {
            return Err(FirLoweringFailure::UnsupportedProgressionMember);
        };
        let receiver_value = self.ir.add_expr(self.ir.expr(value).clone());
        self.external_property_access(
            *property,
            dispatch.clone(),
            *receiver,
            parameters,
            *result,
            *extension_receiver_parameter,
            Some(receiver_value),
            None,
            &[],
            false,
        )
        .ok_or(FirLoweringFailure::UnsupportedExternalProperty(*property))
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

    fn iterator_loop(
        &mut self,
        contract: IteratorLoopContract<'_>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let IteratorLoopContract {
            target,
            variable,
            variable_ty,
            iterable,
            iterator_ty,
            iterator,
            has_next,
            next,
            body,
        } = contract;
        // The checked iterable is the receiver of exactly one selected `iterator()` call. Keep that
        // direct semantic operand instead of introducing state that does not exist in the source.
        let iterable_ty = self
            .body
            .expr(iterable)
            .ok_or(FirLoweringFailure::MissingExpression(iterable))?
            .ty
            .get();
        let iterable_value = self.expression(iterable)?;
        let iterator_value = self.iterator_call(iterator, iterable_value, iterable_ty)?;
        let iterator_slot = self.allocate_temporary();
        let iterator_declaration = self.ir.add_expr(IrExpr::Variable {
            index: iterator_slot,
            ty: iterator_ty.get(),
            init: Some(iterator_value),
            named: false,
        });

        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let condition = self.iterator_call(has_next, iterator_read, iterator_ty.get())?;
        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let element = self.iterator_call(next, iterator_read, iterator_ty.get())?;
        let (_, variable_declaration) =
            self.loop_variable_declaration(variable.raw(), variable_ty.get(), element);
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
            stmts: vec![iterator_declaration, loop_expression],
            value: None,
        }))
    }

    pub(super) fn iterator_call(
        &mut self,
        call: &FirIteratorCall,
        receiver: ExprId,
        receiver_ty: crate::types::Ty,
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
        // The protocol call carries no substitution of its declaration's type parameters, so a
        // scalar receiver of a generic module extension (`fun <T : Any> T?.iterator()`) is boxed
        // into the declared receiver here.
        let receiver = match (&call.target, &call.receiver) {
            (
                crate::fir::FirCallTarget::Module(target),
                FirIteratorReceiver::Extension | FirIteratorReceiver::MemberExtension { .. },
            ) => {
                let declared = self
                    .index
                    .callable(*target)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?
                    .shape
                    .extension_receiver
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?;
                self.box_into_erased_parameter(receiver, receiver_ty, declared.get())
            }
            _ => receiver,
        };
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
                    SameFileExtensionReceiverMode::DirectWhenOrdered,
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
}
