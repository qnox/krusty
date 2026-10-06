use crate::fir::{
    ControlTargetId, FirBuiltinIterableKind, FirExprId, FirIteratorCall, FirIteratorReceiver,
    FirLoopHeader, FirProgressionClass, FirProgressionSource, FirRangeCounterKind,
    FirWithIndexLoop, LocalValueId, OriginId, ResolvedTy,
};
use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrIntrinsic, IrProgressionSource};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

/// What one iteration binds from the element it reads.
#[derive(Clone, Copy)]
pub(super) enum LoopBinding<'a> {
    /// The loop's own variable.
    Variable(LocalValueId),
    /// The destructuring of a `withIndex()` loop, whose entries read the index and the element
    /// (kotlinc's `WithIndexLoopHeader`).
    WithIndex(&'a FirWithIndexLoop),
}

/// The checked pieces of one loop indexing an array or a `CharSequence`.
pub(super) struct IterableLoopContract<'a> {
    pub(super) target: ControlTargetId,
    pub(super) binding: LoopBinding<'a>,
    pub(super) variable_ty: ResolvedTy,
    pub(super) kind: &'a FirBuiltinIterableKind,
    pub(super) iterable: FirExprId,
    pub(super) body: FirExprId,
}

/// The checked pieces of one iterator-protocol loop, kept together across the FIR-to-IR boundary.
pub(super) struct IteratorLoopContract<'a> {
    pub(super) target: ControlTargetId,
    pub(super) binding: LoopBinding<'a>,
    pub(super) variable_ty: ResolvedTy,
    pub(super) iterable: FirExprId,
    pub(super) iterator_ty: ResolvedTy,
    pub(super) iterator: &'a FirIteratorCall,
    pub(super) has_next: &'a FirIteratorCall,
    pub(super) next: &'a FirIteratorCall,
    pub(super) body: FirExprId,
}

/// The checked semantic pieces of one counted loop over a progression. Common lowering preserves
/// this contract; each backend chooses its own counted-loop control-flow shape.
pub(super) struct RangeLoopContract<'a> {
    pub(super) target: ControlTargetId,
    pub(super) binding: LoopBinding<'a>,
    pub(super) counter: FirRangeCounterKind,
    pub(super) source: std::borrow::Cow<'a, FirProgressionSource>,
    pub(super) unsigned_compare: Option<&'a crate::fir::FirRuntimeFunction>,
    pub(super) body: FirExprId,
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
        // A destructuring loop's container is compiler-generated, not a named source variable.
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: variable,
            ty,
            init: Some(initializer),
            named: !self.body.is_destructuring_loop_container(source),
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
                unsigned_compare,
            } => self.range_loop(RangeLoopContract {
                target,
                binding: LoopBinding::Variable(*variable),
                counter: *counter,
                source: std::borrow::Cow::Owned(FirProgressionSource::Literal {
                    operation: *operation,
                    start: *start,
                    end: *end,
                }),
                unsigned_compare: unsigned_compare.as_ref(),
                body,
            }),
            FirLoopHeader::Progression {
                variable,
                counter,
                source,
                unsigned_compare,
            } => self.range_loop(RangeLoopContract {
                target,
                binding: LoopBinding::Variable(*variable),
                counter: *counter,
                source: std::borrow::Cow::Borrowed(source),
                unsigned_compare: unsigned_compare.as_ref(),
                body,
            }),
            FirLoopHeader::Iterable {
                variable,
                variable_ty,
                kind,
                iterable,
            } => self.iterable_loop(IterableLoopContract {
                target,
                binding: LoopBinding::Variable(*variable),
                variable_ty: *variable_ty,
                kind,
                iterable: *iterable,
                body,
            }),
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
                binding: LoopBinding::Variable(*variable),
                variable_ty: *variable_ty,
                iterable: *iterable,
                iterator_ty: *iterator_ty,
                iterator,
                has_next,
                next,
                body,
            }),
            FirLoopHeader::WithIndex(with_index) => self.with_index_loop(target, with_index, body),
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

    pub(super) fn range_loop(
        &mut self,
        lp: RangeLoopContract<'_>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let source = self.progression_source(&lp.source)?;
        let (variable, with_index) = match lp.binding {
            LoopBinding::Variable(variable) => (variable, None),
            LoopBinding::WithIndex(with_index) => {
                let (element, index) = self.counted_with_index(with_index)?;
                (element, Some(index))
            }
        };
        let body = self.expression(lp.body)?;
        Ok(self
            .ir
            .add_expr(IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop {
                variable: self.value_slot(variable),
                variable_name: self.body.debug_value_name(variable).map(Into::into),
                counter: lp.counter.ty(),
                source,
                unsigned_compare: lp.unsigned_compare.map(runtime_function),
                body,
                label: self.control_label(0, lp.target)?,
                with_index,
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
                last_element: runtime_function(last_element),
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
        let first = self.member_property_read(&progression.first, value)?;
        let last = self.member_property_read(&progression.last, value)?;
        let step = progression
            .step
            .as_ref()
            .map(|step| self.member_property_read(step, value))
            .transpose()?;
        Ok(IrProgressionSource::Value {
            setup,
            first,
            last,
            step,
        })
    }

    /// A read of a selected member property on the stored receiver `value` (a leaf read, re-read
    /// for each read).
    pub(super) fn member_property_read(
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

            constant_value_field: _,
        } = target
        else {
            return Err(FirLoweringFailure::UnsupportedLoopMember);
        };
        let receiver_value = self.ir.add_expr(self.ir.expr(value).clone());
        self.external_property_access(super::source_calls::ExternalPropertyRequest {
            target: *property,
            dispatch: dispatch.clone(),
            receiver_ty: *receiver,
            parameters,
            result: *result,
            extension_receiver_parameter: *extension_receiver_parameter,
            dispatch_receiver: super::source_calls::DispatchOperand {
                receiver: Some(receiver_value),
                class: Self::static_class(*receiver),
            },
            extension_receiver: None,
            arguments: &[],
            write: false,
        })
        .ok_or(FirLoweringFailure::UnsupportedExternalProperty(*property))
    }

    pub(super) fn iterable_loop(
        &mut self,
        contract: IterableLoopContract<'_>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let IterableLoopContract {
            target,
            binding,
            variable_ty,
            kind,
            iterable,
            body,
        } = contract;
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
        let iterable_value = self.expression(iterable)?;
        let stable_slot = stable_local.and_then(|value| {
            matches!(self.ir.expr(iterable_value), IrExpr::GetValue(slot) if *slot == self.value_slot(value))
                .then_some(self.value_slot(value))
        });
        // The receiver each read re-reads: a stable local, the constant itself (kotlinc's
        // `JvmOptimizationLowering` inlines a temporary `val` holding a constant), or a temporary.
        let (receiver_leaf, iterable_declaration) = if let Some(slot) = stable_slot {
            (self.ir.add_expr(IrExpr::GetValue(slot)), None)
        } else if matches!(self.ir.expr(iterable_value), IrExpr::Const(_)) {
            (iterable_value, None)
        } else {
            let slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: iterable_ty,
                init: Some(iterable_value),
                named: false,
            });
            (self.ir.add_expr(IrExpr::GetValue(slot)), Some(declaration))
        };
        // A `withIndex()` loop counts its index with this counter, which starts at 0 and steps by 1.
        let index_slot = match binding {
            LoopBinding::Variable(_) => self.allocate_temporary(),
            LoopBinding::WithIndex(with_index) => self.value_slot(with_index.index),
        };
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let index_declaration = self.ir.add_expr(IrExpr::Variable {
            index: index_slot,
            ty: Ty::Int,
            init: Some(zero),
            named: false,
        });
        let (size_declaration, condition, element) = match kind {
            FirBuiltinIterableKind::Array | FirBuiltinIterableKind::String => {
                let (size_operation, get_operation) = match kind {
                    FirBuiltinIterableKind::String => {
                        (IrIntrinsic::StringLength, IrIntrinsic::StringGet)
                    }
                    _ => (IrIntrinsic::ArraySize, IrIntrinsic::ArrayGet),
                };
                let receiver = self.ir.add_expr(self.ir.expr(receiver_leaf).clone());
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
                let receiver = self.ir.add_expr(self.ir.expr(receiver_leaf).clone());
                let index_read = self.ir.add_expr(IrExpr::GetValue(index_slot));
                let element = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: get_operation,
                        ret: variable_ty.get(),
                    },
                    dispatch_receiver: Some(receiver),
                    args: vec![index_read],
                });
                (Some(size_declaration), condition, element)
            }
            FirBuiltinIterableKind::CharSequence(indexing) => {
                let (condition, element) =
                    self.char_sequence_indexing(indexing, receiver_leaf, iterable_ty, index_slot)?;
                (None, condition, element)
            }
        };
        let mut iteration = self.iteration(binding, variable_ty, element, false)?;
        iteration.push(self.expression(body)?);
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: iteration,
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
        statements.push(index_declaration);
        statements.extend(size_declaration);
        statements.push(loop_expression);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        }))
    }

    pub(super) fn iterator_loop(
        &mut self,
        contract: IteratorLoopContract<'_>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let IteratorLoopContract {
            target,
            binding,
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
        let mut iterable_value = self.expression(iterable)?;
        let mut iterable_ty = iterable_ty;
        if let LoopBinding::WithIndex(_) = binding {
            (iterable_value, iterable_ty) =
                self.with_index_receiver(iterable_value, iterable_ty, iterator);
        }
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
        let element = match binding {
            LoopBinding::WithIndex(with_index) if !with_index.reads_value() => self.iterator_call(
                &Self::undeclared_next(next),
                iterator_read,
                iterator_ty.get(),
            )?,
            _ => self.iterator_call(next, iterator_read, iterator_ty.get())?,
        };
        // A `withIndex()` loop declares its own index after the iterator.
        let index_declaration = match binding {
            LoopBinding::Variable(_) => None,
            LoopBinding::WithIndex(with_index) => Some(self.index_declaration(with_index)),
        };
        let mut iteration = self.iteration(binding, variable_ty, element, true)?;
        iteration.push(self.expression(body)?);
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: iteration,
            value: None,
        });
        let loop_expression = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: None,
            post_test: false,
            label: Some(self.control_label(0, target)?),
        });
        // kotlinc rebuilds only a `withIndex()` loop over an iterator, whose body then declares
        // its variables in the loop's own scope.
        if index_declaration.is_some() {
            self.ir.transparent_loop_bodies.insert(loop_expression);
        }
        let mut statements = vec![iterator_declaration];
        statements.extend(index_declaration);
        statements.push(loop_expression);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        }))
    }

    /// The declarations opening one iteration that reads `element`: the loop variable, or the
    /// entries of a `withIndex()` loop's destructuring, where `counts_index` says the loop steps
    /// an index of its own rather than reading its counter.
    fn iteration(
        &mut self,
        binding: LoopBinding<'_>,
        variable_ty: ResolvedTy,
        element: ExprId,
        counts_index: bool,
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        match binding {
            LoopBinding::Variable(variable) => {
                let (_, declaration) =
                    self.loop_variable_declaration(variable.raw(), variable_ty.get(), element);
                Ok(vec![declaration])
            }
            LoopBinding::WithIndex(with_index) => {
                self.with_index_iteration(with_index, element, counts_index)
            }
        }
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
        let receiver =
            self.lowered_with_conversion(receiver, receiver_ty, call.receiver_conversion)?;
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
        let result = self.loop_call(
            &call.target,
            dispatch_receiver,
            extension_receiver,
            &arguments,
            &context_parameter_types,
        )?;
        let Some(check) = call.result_check else {
            return Ok(result);
        };
        let check = self
            .body
            .platform_narrowing(check)
            .map(super::expression::platform_null_check)
            .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)?;
        Ok(self.ir.add_expr(IrExpr::NotNullAssert {
            operand: result,
            check,
        }))
    }

    /// A call a loop makes on its own to a selected `target`, with its receivers and arguments
    /// already lowered.
    pub(super) fn loop_call(
        &mut self,
        target: &crate::fir::FirCallTarget,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[crate::ir::IrCheckedArgument],
        context_parameter_types: &[Ty],
    ) -> Result<ExprId, FirLoweringFailure> {
        match target {
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
                    super::source_calls::DispatchOperand::plain(dispatch_receiver),
                    extension_receiver,
                    arguments,
                    context_parameter_types,
                    &[],
                    None,
                )
                .unwrap_or(Err(FirLoweringFailure::MissingCallable(*target))),
            crate::fir::FirCallTarget::External {
                declaration,
                default_provider,
                receiver,
                declared_receiver,
                parameters,
                result,
                declared_result,
                overridden_results,
                semantic_role,
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
                    overridden_results,
                    semantic_role: *semantic_role,
                    suspend: *suspend,
                    can_inline: *can_inline,
                    inline_plan: inline_plan.as_deref(),
                    substitutions: &[],
                    extension_receiver_parameter: *extension_receiver_parameter,
                    dispatch_receiver,
                    dispatch_class: Self::static_class(*receiver),
                    extension_receiver,
                    arguments,
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
                    arguments,
                )
                .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall),
        }
    }
}

fn runtime_function(function: &crate::fir::FirRuntimeFunction) -> crate::ir::IrRuntimeFunction {
    crate::ir::IrRuntimeFunction {
        function: function.function,
        parameters: function.parameters.to_vec(),
        result: function.result,
    }
}
