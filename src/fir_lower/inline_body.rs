//! Lowering of complete checked inline-body contracts.

use crate::fir::ResolvedTy;
use crate::ir::{Callee, ExprId, IrCheckedArgument, IrConst, IrExpr};
use crate::types::Ty;

use super::source_calls::{
    inline_argument_template, rehome_inline_body_values, SelectedDefaultMode, SelectedOperandMode,
    SelectedOperandRequest,
};
use super::BodyLowering;

/// The first line a body's emission marks: its own, or the first position a block reaches.
/// An `@InlineOnly` expansion whose lambda starts on the call's line separates the two with the
/// synthetic inline line, so the marker's attribution is decided from this line.
fn first_body_line(ir: &crate::ir::IrFile, body: ExprId) -> Option<u32> {
    let mut expression = body;
    loop {
        if let Some(&line) = ir.expr_lines.get(&expression) {
            return (line != 0).then_some(line);
        }
        let IrExpr::Block { stmts, value } = ir.expr(expression) else {
            return None;
        };
        expression = *stmts.first().or(value.as_ref())?;
    }
}

/// The IR record of one mapped body line of an external inline declaration: the dependency's
/// line and the call-site line it answers to.
fn external_frame_line(
    source: &crate::fir::FirInlineBodySource,
    line: u32,
    call_line: u32,
) -> crate::ir::IrExternalFrameLine {
    crate::ir::IrExternalFrameLine {
        file: source.file.clone(),
        path: source.path.clone(),
        line,
        call_line,
    }
}

pub(super) struct ExternalInlineCallRequest<'a> {
    pub(super) plan: &'a crate::fir::FirInlineBodyPlan,
    pub(super) receiver_ty: Option<ResolvedTy>,
    pub(super) parameter_types: &'a [Ty],
    pub(super) result: ResolvedTy,
    pub(super) dispatch_receiver: Option<ExprId>,
    pub(super) extension_receiver: Option<ExprId>,
    pub(super) arguments: &'a [IrCheckedArgument],
    /// Source line of the expanded call. The frame's marker and frame-closing line attribute to
    /// it; `None` lowers the same expansion without that attribution.
    pub(super) source_line: Option<u32>,
}

/// The declaration-plan inputs of an external inline iteration. They travel together from the
/// checked plan match to the lowering entry, grouped so neither entry needs an argument-count
/// suppression.
pub(super) struct ExternalIterationPlan<'a> {
    pub(super) frame: &'a crate::fir::FirInlineBodyFrame,
    pub(super) lambda_parameter: u32,
    pub(super) element_ty: ResolvedTy,
    pub(super) index: Option<&'a crate::fir::FirInlineIterationIndex>,
    pub(super) traversal: &'a crate::fir::FirInlineIterationTraversal,
    /// Source line of the expanded call; the frame's mapped body lines answer to it.
    pub(super) source_line: Option<u32>,
}

/// The declaration-plan inputs of an external inline collection transform.
pub(super) struct ExternalCollectionTransformPlan<'a> {
    pub(super) lambda_parameter: u32,
    pub(super) local_names: &'a crate::fir::FirInlineCollectionLocalNames,
    pub(super) traversal: &'a crate::fir::FirInlineIterationTraversal,
    pub(super) factory: crate::fir::ExternalCallableId,
    pub(super) factory_classifier: crate::types::TypeName,
    pub(super) factory_parameters: &'a [ResolvedTy],
    pub(super) capacity: Option<&'a crate::fir::FirInlineCollectionCapacity>,
    pub(super) append: &'a crate::fir::FirInlineCollectionAppend,
    pub(super) accumulator_ty: ResolvedTy,
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
            source_line,
        } = request;
        let (
            frame,
            lambda_parameter,
            invocation_arguments,
            prologue,
            cleanup,
            cause_ty,
            recovery,
            plan_defaults,
            returned_value,
            declared_receiver,
        ) = match plan {
            crate::fir::FirInlineBodyPlan::Iteration {
                frame,
                lambda_parameter,
                element,
                index,
                traversal,
            } => {
                return self.external_inline_iteration(
                    ExternalIterationPlan {
                        frame,
                        lambda_parameter: *lambda_parameter,
                        element_ty: *element,
                        index: index.as_ref(),
                        traversal,
                        source_line,
                    },
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
                    ExternalCollectionTransformPlan {
                        lambda_parameter: *lambda_parameter,
                        local_names,
                        traversal,
                        factory: *factory,
                        factory_classifier: *factory_classifier,
                        factory_parameters,
                        capacity: capacity.as_ref(),
                        append: append.as_ref(),
                        accumulator_ty: *accumulator,
                    },
                    receiver_ty,
                    parameter_types,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::InvokeLambda {
                frame,
                lambda_parameter,
                arguments,
                prologue,
                cleanup,
                cause,
                recovery,
                defaults,
                result: returned,
                declared_receiver,
            } => (
                frame,
                *lambda_parameter,
                arguments,
                prologue,
                cleanup,
                *cause,
                recovery.as_deref(),
                defaults,
                *returned,
                *declared_receiver,
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
        self.record_inlined_type_parameter_receiver(&statements, receiver, declared_receiver);
        self.reuse_plain_function_argument(&mut statements, &mut args, lambda_parameter);
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
        let (inline_body, callee_receiver) = self.materialize_external_inline_lambda(
            &mut statements,
            &args,
            lambda_parameter,
            *parameter_types.get(lambda_parameter)?,
            &invocation_operands,
            frame,
            source_line,
            !cleanup.is_empty(),
        )?;

        let value = if let Some(recovery) = recovery {
            if !cleanup.is_empty() || cause.is_some() {
                return None;
            }
            self.recover_inline_body(inline_body, recovery, result.get())?
        } else if cleanup.is_empty() {
            inline_body
        } else {
            self.guard_inline_body(
                inline_body,
                cleanup,
                cause,
                result.get(),
                source_line,
                plan_value,
            )?
        };
        let expansion = if let Some(returned_value) = returned_value {
            statements.push(value);
            // A plan returning its receiver reads the inlined callee's receiver parameter. The
            // lambda receiver is a separate semantic parameter initialized from that value.
            let returned = match (returned_value, callee_receiver) {
                (crate::fir::FirInlineValue::Receiver, Some(receiver)) => (
                    self.ir.add_expr(IrExpr::GetValue(receiver)),
                    receiver_ty?.get(),
                ),
                _ => plan_value(self, returned_value)?,
            };
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(returned.0),
            })
        } else if statements.is_empty() {
            value
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(value),
            })
        };
        if matches!(self.ir.expr(expansion), IrExpr::Block { .. }) {
            if let Some(line) = source_line {
                self.ir.expr_source_lines.insert(expansion, line);
            }
        }
        self.ir.external_inline_expansions.insert(expansion);
        Some(expansion)
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
            catches: vec![crate::ir::IrCatch::generated(
                caught,
                caught_internal,
                failed,
            )],
            finally: None,
            result: result_ty,
        }))
    }

    pub(super) fn external_inline_iteration(
        &mut self,
        plan: ExternalIterationPlan<'_>,
        receiver_ty: Option<ResolvedTy>,
        parameter_types: &[Ty],
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let ExternalIterationPlan {
            frame,
            lambda_parameter,
            element_ty,
            index,
            traversal,
            source_line: call_line,
        } = plan;
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
        let mut iterable = receiver?;
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
        let (implementation, captures, inline_body, arity) =
            inline_argument_template(self.ir, lambda)?;
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
            // An inlined lambda's plain-local capture reads the caller's local in place:
            // kotlinc's inliner remaps the read instead of copying the value into a frame
            // temporary the loop would have to keep alive.
            if let IrExpr::GetValue(slot) = self.ir.expr(capture) {
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
        self.ir
            .call_operand_bindings
            .extend(capture_declarations.iter().copied());
        statements.splice(
            declaration_position..declaration_position,
            capture_declarations,
        );
        let capture_count = formal_slots.len();

        let frame_source = frame.source.clone();
        if frame_source.is_some() {
            // The expansion reproduces the reference compiler's frame for the call: the receiver
            // operand's local is the frame's named `$this$` local (the already-declared operand
            // temp renamed, or a copy where the operand was never spilled), and the function
            // marker opens the frame once every operand is bound.
            let role = if extension_receiver.is_some() {
                crate::ir::IrInlineLocalRole::ExtensionReceiver
            } else {
                crate::ir::IrInlineLocalRole::DispatchReceiver
            };
            let declared = match self.ir.expr(iterable) {
                IrExpr::GetValue(slot) => statements.iter().copied().find(|statement| {
                    matches!(
                        self.ir.expr(*statement),
                        IrExpr::Variable { index, .. } if index == slot
                    )
                }),
                _ => None,
            };
            let declaration = match declared {
                Some(declaration) => declaration,
                None => {
                    let slot = self.allocate_temporary();
                    let declaration = self.ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty: receiver_ty?.get(),
                        init: Some(iterable),
                        named: true,
                    });
                    self.ir.call_operand_bindings.insert(declaration);
                    statements.push(declaration);
                    iterable = self.ir.add_expr(IrExpr::GetValue(slot));
                    declaration
                }
            };
            if let IrExpr::Variable { named, .. } = &mut self.ir.exprs[declaration as usize] {
                *named = true;
            }
            self.ir
                .value_names
                .insert(declaration, frame.callee.to_string());
            self.ir.set_debug_local_provenance(
                declaration,
                crate::ir::IrDebugLocalProvenance::inline_value(role, 1),
            );
            let marker = self.inline_marker(
                frame.callee.to_string(),
                crate::ir::IrDebugLocalProvenance::FunctionFrameMarker,
            );
            statements.push(marker);
        }

        let action_index_slot = index.map(|_| self.allocate_temporary());
        let current_index_slot = index.map(|_| self.allocate_temporary());
        if let Some(current_index_slot) = current_index_slot {
            formal_slots.push(current_index_slot);
        }
        let element_slot = self.allocate_temporary();
        // With a resolved frame the body's invocation operand is the lambda parameter's own
        // local (`it`), bound from the frame's raw element local inside the lambda's frame; the
        // legacy shape binds the element local itself.
        let it_slot = if frame_source.is_some() {
            let slot = self.allocate_temporary();
            formal_slots.push(slot);
            Some(slot)
        } else {
            formal_slots.push(element_slot);
            None
        };
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
        let (condition, element, element_raw_ty, update, loop_identity) = match traversal {
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
                let iterator_declaration = self.ir.add_expr(IrExpr::Variable {
                    index: iterator_slot,
                    ty: iterator_ty,
                    init: Some(current),
                    named: false,
                });
                statements.push(iterator_declaration);
                if let (Some(source), Some(call_line)) = (&frame_source, call_line) {
                    // The frame's first body statement marks the body's invocation line, mapped
                    // under the call; the rest of the prelude inherits it until the lambda runs.
                    let line = u32::from(source.invoke_line);
                    self.ir.expr_lines.insert(iterator_declaration, line);
                    self.ir.external_frame_lines.insert(
                        iterator_declaration,
                        external_frame_line(source, line, call_line),
                    );
                }
                let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
                let condition =
                    self.external_inline_iteration_member_call(has_next, iterator_read, &[])?;
                let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
                // The frame's element local holds the traversal's raw result: the erased
                // `next()` lands unadapted and the lambda parameter's own local adapts it. The
                // legacy shape consumes the applied element type at the call.
                let (element, element_raw) = if frame_source.is_some() {
                    let element = self.external_inline_iteration_member_call_typed(
                        next,
                        iterator_read,
                        &[],
                        next.physical_result,
                    )?;
                    (element, next.physical_result.get())
                } else {
                    let element =
                        self.external_inline_iteration_member_call(next, iterator_read, &[])?;
                    (element, element_ty.get())
                };
                (condition, element, element_raw, None, iterator_slot)
            }
            crate::fir::FirInlineIterationTraversal::Array
            | crate::fir::FirInlineIterationTraversal::Counted { .. } => {
                let receiver_slot = self.allocate_temporary();
                let receiver_declaration = self.ir.add_expr(IrExpr::Variable {
                    index: receiver_slot,
                    ty: receiver_ty?.get(),
                    init: Some(iterable),
                    named: false,
                });
                self.ir.call_operand_bindings.insert(receiver_declaration);
                statements.push(receiver_declaration);
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
                let incremented =
                    self.ir
                        .add_arithmetic(crate::ir::IrBinOp::Add, cursor, one, Ty::Int);
                let update = self.ir.add_expr(IrExpr::SetValue {
                    var: cursor_slot,
                    value: incremented,
                });
                let element_raw = match traversal {
                    crate::fir::FirInlineIterationTraversal::Array => element_ty.get(),
                    crate::fir::FirInlineIterationTraversal::Counted { get, .. } => {
                        if frame_source.is_some() {
                            get.physical_result.get()
                        } else {
                            element_ty.get()
                        }
                    }
                    crate::fir::FirInlineIterationTraversal::Iterator { .. } => unreachable!(),
                };
                (condition, element, element_raw, Some(update), cursor_slot)
            }
        };
        let loop_label = format!("$fir_inline_iteration_{loop_identity}");
        let element_declaration = self.ir.add_expr(IrExpr::Variable {
            index: element_slot,
            ty: element_raw_ty,
            init: Some(element),
            named: true,
        });
        if let Some(source) = &frame_source {
            // The frame's element local holds the traversal's raw result (an iterator's erased
            // `next()`); the lambda parameter's own local adapts it where the lambda binds.
            if let Some(element) = &source.element {
                self.ir
                    .value_names
                    .insert(element_declaration, element.to_string());
            }
            self.ir.set_debug_local_provenance(
                element_declaration,
                crate::ir::IrDebugLocalProvenance::inline_value(
                    crate::ir::IrInlineLocalRole::Value,
                    1,
                ),
            );
        }
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
            let incremented =
                self.ir
                    .add_arithmetic(crate::ir::IrBinOp::Add, current, one, Ty::Int);
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
        let body_tail = match (&frame_source, it_slot) {
            (Some(source), Some(it_slot)) => {
                let value_parameter = capture_count + usize::from(index.is_some());
                let source_name = self
                    .ir
                    .function_parameter_identities(implementation)
                    .and_then(|identities| identities.get(value_parameter))
                    .and_then(|identity| identity.source_name.clone());
                let read = self.ir.add_expr(IrExpr::GetValue(element_slot));
                // The frame's raw element local holds the traversal's erased ABI result; the
                // lambda parameter's own local consumes it at the applied element type. A
                // reference element crosses through the class's box — the cast names it, which
                // is what the value-class representation boundary reads to unbox a value-class
                // element here — while a scalar element's coercion unboxes directly.
                let bound = if element_ty.get().is_reference() {
                    let cast = self.ir.add_expr(IrExpr::TypeOp {
                        op: crate::ir::IrTypeOp::Cast,
                        arg: read,
                        type_operand: element_ty.get(),
                    });
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: crate::ir::IrTypeOp::ImplicitCoercion,
                        arg: cast,
                        type_operand: element_ty.get(),
                    })
                } else {
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: crate::ir::IrTypeOp::ImplicitCoercion,
                        arg: read,
                        type_operand: element_ty.get(),
                    })
                };
                let parameter_declaration = self.ir.add_expr(IrExpr::Variable {
                    index: it_slot,
                    ty: element_ty.get(),
                    init: Some(bound),
                    named: true,
                });
                self.ir.call_operand_bindings.insert(parameter_declaration);
                if let Some(name) = source_name {
                    self.ir.value_names.insert(parameter_declaration, name);
                }
                self.ir.set_debug_local_provenance(
                    parameter_declaration,
                    crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { depth: 0 },
                );
                let marker = self.inline_marker(
                    frame.callee.to_string(),
                    crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
                        implementation,
                        depth: 0,
                    },
                );
                let lambda_frame = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![parameter_declaration, marker, inline_body],
                    value: None,
                });
                if let Some(call_line) = call_line {
                    // The lambda frame closes on the body's invocation line, mapped under the
                    // call; the mark drives the frame's closing `nop`.
                    let line = u32::from(source.invoke_line);
                    self.ir.expr_source_lines.insert(lambda_frame, line);
                    self.ir
                        .external_frame_lines
                        .insert(lambda_frame, external_frame_line(source, line, call_line));
                }
                lambda_frame
            }
            _ => inline_body,
        };
        body_statements.push(body_tail);
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
        let expansion = self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(unit),
        });
        if let Some(source) = &frame_source {
            self.ir.external_inline_expansions.insert(expansion);
            if let Some(call_line) = call_line {
                self.ir.external_frame_closes.insert(
                    expansion,
                    external_frame_line(source, u32::from(source.close_line), call_line),
                );
            }
        }
        Some(expansion)
    }

    pub(super) fn external_inline_iteration_member_call(
        &mut self,
        call: &crate::fir::FirInlineIterationMemberCall,
        receiver: ExprId,
        arguments: &[(ExprId, Ty)],
    ) -> Option<ExprId> {
        self.external_inline_iteration_member_call_typed(call, receiver, arguments, call.result)
    }

    /// The same convention call, consumed at `result` rather than at the member's applied result:
    /// an inlined frame's raw local holds the declaration's erased ABI result.
    fn external_inline_iteration_member_call_typed(
        &mut self,
        call: &crate::fir::FirInlineIterationMemberCall,
        receiver: ExprId,
        arguments: &[(ExprId, Ty)],
        result: ResolvedTy,
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
            result,
            declared_result: None,
            overridden_results: &[],
            semantic_role: None,
            overridden_declarations: &[],
            suspend: false,
            can_inline: false,
            inline_plan: None,
            substitutions: &[],
            extension_receiver_parameter: None,
            dispatch_receiver: Some(receiver),
            dispatch_class: Self::static_class(Some(call.receiver)),
            extension_receiver: None,
            arguments: &arguments,
            source_line: None,
        })
    }

    /// The spilled extension receiver of an inlined type parameter keeps the declaration type.
    /// Emission stores that parameter as its erased bound and casts back only where a use needs
    /// the call-site class.
    fn record_inlined_type_parameter_receiver(
        &mut self,
        statements: &[ExprId],
        receiver: Option<ExprId>,
        declared: Option<crate::fir::ResolvedTy>,
    ) {
        let Some(declared) = declared
            .map(crate::fir::ResolvedTy::get)
            .filter(|receiver| receiver.non_null().is_ty_param())
        else {
            return;
        };
        let Some(receiver) = receiver else {
            return;
        };
        let IrExpr::GetValue(slot) = *self.ir.expr(receiver) else {
            return;
        };
        let Some(&declaration) = statements.iter().find(|statement| {
            matches!(
                self.ir.expr(**statement),
                IrExpr::Variable { index, .. } if *index == slot
            )
        }) else {
            return;
        };
        self.ir.record_inline_declared_type(declaration, declared);
    }

    /// A function argument that is already a local is invoked from that local.
    ///
    /// The plan materializes every operand so a lambda literal can be found and spliced. A plain
    /// local has no template: the copy would be a second slot the reference compiler does not
    /// emit, and the invocation reads the caller's parameter directly.
    fn reuse_plain_function_argument(
        &mut self,
        statements: &mut Vec<ExprId>,
        args: &mut [ExprId],
        lambda_parameter: usize,
    ) {
        let Some(&function) = args.get(lambda_parameter) else {
            return;
        };
        let IrExpr::GetValue(slot) = *self.ir.expr(function) else {
            return;
        };
        let Some(position) = statements.iter().position(|statement| {
            matches!(
                self.ir.expr(*statement),
                IrExpr::Variable {
                    index,
                    init: Some(init),
                    ..
                } if *index == slot && matches!(self.ir.expr(*init), IrExpr::GetValue(_))
            )
        }) else {
            return;
        };
        let reads = args
            .iter()
            .filter(
                |&&operand| matches!(self.ir.expr(operand), IrExpr::GetValue(read) if *read == slot),
            )
            .count();
        if reads != 1 {
            return;
        }
        let IrExpr::Variable {
            init: Some(init), ..
        } = *self.ir.expr(statements[position])
        else {
            return;
        };
        args[lambda_parameter] = init;
        statements.remove(position);
    }

    /// An erased `FunctionN.invoke`, including a `Unit` specialization.
    ///
    /// The invoke stores `Object` inside the `try`. `finally` runs, then the normal path reloads
    /// that object and jumps over the handlers. A used result is narrowed at that landing. A
    /// `Unit` specialization pops there: the load and the pop are not adjacent, so temporary
    /// elimination keeps the store and pop-backward removes the reload.
    fn guard_erased_invocation_result(
        &mut self,
        body: ExprId,
        cleanup: &[crate::fir::FirInlineCall],
        cause: Option<(u32, Ty)>,
        result_ty: Ty,
        plan_value: impl Fn(&mut Self, crate::fir::FirInlineValue) -> Option<(ExprId, Ty)> + Copy,
    ) -> Option<ExprId> {
        let result_slot = self.allocate_temporary();
        let label = format!("$inline_invoke${result_slot}");
        let initial = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: result_slot,
            ty: Ty::obj("java/lang/Object"),
            init: Some(initial),
            named: false,
        });
        self.ir
            .inline_return_frames
            .insert(declaration, label.clone());
        let store_result = self.ir.add_expr(IrExpr::SetValue {
            var: result_slot,
            value: body,
        });
        let exit = self.ir.add_expr(IrExpr::Break {
            label: Some(label.clone()),
        });
        let try_body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![store_result, exit],
            value: None,
        });
        let guarded = self.guarded_try(try_body, cleanup, cause, Ty::Unit, plan_value)?;
        let loop_body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![guarded],
            value: None,
        });
        let condition = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let carried = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(label),
        });
        let read = self.ir.add_expr(IrExpr::GetValue(result_slot));
        let narrowed = self.ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg: read,
            type_operand: result_ty,
        });
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, carried],
            value: Some(narrowed),
        }))
    }

    fn guard_inline_body(
        &mut self,
        body: ExprId,
        cleanup: &[crate::fir::FirInlineCall],
        cause: Option<(u32, Ty)>,
        result_ty: Ty,
        source_line: Option<u32>,
        plan_value: impl Fn(&mut Self, crate::fir::FirInlineValue) -> Option<(ExprId, Ty)> + Copy,
    ) -> Option<ExprId> {
        // `FunctionN.invoke` returns erased `Object`, including when the function type is `Unit`.
        // Both specializations reload after `finally` and jump over the handlers. A used result
        // is narrowed at that landing; a `Unit` result is popped there.
        let erased_invoke = matches!(
            self.ir.expr(body),
            IrExpr::InvokeFunction { ret, .. } if *ret != Ty::Nothing
        );
        if erased_invoke {
            return self
                .guard_erased_invocation_result(body, cleanup, cause, result_ty, plan_value);
        }
        // The protected expression has a semantic result. Keep it as the `try` result instead of
        // inventing a source-absent loop and local solely to prescribe a JVM store/reload shape.
        // Backends decide how that value crosses the cleanup; the JVM uses the provenance marker
        // below to reproduce kotlinc's physical temporary and landing sequence.
        let guarded = self.guarded_try(body, cleanup, cause, result_ty, plan_value)?;
        if let Some(line) = source_line {
            self.ir.expr_source_lines.insert(guarded, line);
        }
        self.ir.inline_cleanup_results.insert(guarded);
        Some(guarded)
    }

    fn guarded_try(
        &mut self,
        body: ExprId,
        cleanup: &[crate::fir::FirInlineCall],
        cause: Option<(u32, Ty)>,
        result: Ty,
        plan_value: impl Fn(&mut Self, crate::fir::FirInlineValue) -> Option<(ExprId, Ty)> + Copy,
    ) -> Option<ExprId> {
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
                let handler = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![record, rethrow],
                    value: None,
                });
                vec![crate::ir::IrCatch::generated(
                    caught,
                    cause_ty.non_null().obj_internal()?,
                    handler,
                )]
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
        Some(self.ir.add_expr(IrExpr::Try {
            body,
            catches,
            finally: Some(finally),
            result,
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
    ///
    /// The expansion reproduces the reference compiler's frame for the call: each invocation
    /// operand becomes a local of the frame (the inlined callee's receiver parameter initializes
    /// the lambda's distinct named `$this$` parameter; a value parameter's operand becomes the
    /// parameter's named local directly), then the lambda-argument marker opens the frame,
    /// attributed to the call's line — or to the synthetic inline line of an `@InlineOnly`
    /// declaration whose lambda body starts on that same line.
    fn materialize_external_inline_lambda(
        &mut self,
        statements: &mut Vec<ExprId>,
        args: &[ExprId],
        lambda_parameter: usize,
        lambda_ty: Ty,
        invocation_operands: &[(ExprId, Ty)],
        frame: &crate::fir::FirInlineBodyFrame,
        call_line: Option<u32>,
        preamble_in_body: bool,
    ) -> Option<(ExprId, Option<u32>)> {
        let function = *args.get(lambda_parameter)?;
        let invoke_function = |lowering: &mut Self| {
            lowering
                .invoke_external_function_value(function, lambda_ty, invocation_operands)
                .map(|invocation| (invocation, None))
        };
        let lambda_slot = match self.ir.expr(function) {
            IrExpr::GetValue(slot) => *slot,
            _ => return invoke_function(self),
        };
        let Some(declaration_position) = statements.iter().position(|statement| {
            matches!(
                self.ir.expr(*statement),
                IrExpr::Variable { index, .. } if *index == lambda_slot
            )
        }) else {
            return invoke_function(self);
        };
        let lambda = match self.ir.expr(statements[declaration_position]).clone() {
            IrExpr::Variable {
                init: Some(lambda), ..
            } => lambda,
            _ => return invoke_function(self),
        };
        let (implementation, captures, inline_body, arity) =
            match inline_argument_template(self.ir, lambda) {
                Some(template) => template,
                None => return invoke_function(self),
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
            // A capture that is already a plain local reads that local. kotlinc's inliner
            // remaps the load; copying it would be a second live slot the caller never names.
            if let IrExpr::GetValue(slot) = self.ir.expr(capture) {
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
        self.ir
            .call_operand_bindings
            .extend(capture_declarations.iter().copied());
        statements.splice(
            declaration_position..declaration_position,
            capture_declarations,
        );
        let capture_count = u32::try_from(formal_slots.len()).ok()?;
        let mut callee_receiver = None;
        // The lambda's parameters and its `$i$a$` marker are inside the protected region, after
        // the `try`'s opening `nop`. They are not statements of the call that precede it.
        let mut preamble = Vec::new();
        let receiver_parameter = self
            .ir
            .lambda_origins
            .get(&implementation)
            .and_then(|origin| origin.receiver_parameter);
        let parameter_identities = self
            .ir
            .function_parameter_identities(implementation)
            .map(<[crate::ir::IrParameterIdentity]>::to_vec);
        for (operand, &(value, ty)) in invocation_operands.iter().enumerate() {
            let parameter = capture_count + u32::try_from(operand).ok()?;
            if receiver_parameter == Some(parameter) {
                // The inlined callee's receiver and the invoked extension lambda's receiver are
                // distinct semantic parameters, even though both receive the same value.
                let callee_slot = self.allocate_temporary();
                statements.push(self.ir.add_expr(IrExpr::Variable {
                    index: callee_slot,
                    ty,
                    init: Some(value),
                    named: false,
                }));
                callee_receiver = Some(callee_slot);
                let slot = self.allocate_temporary();
                let read = self.ir.add_expr(IrExpr::GetValue(callee_slot));
                let declaration = self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty,
                    init: Some(read),
                    named: true,
                });
                self.ir.call_operand_bindings.insert(declaration);
                self.ir.set_debug_local_provenance(
                    declaration,
                    crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver { implementation },
                );
                preamble.push(declaration);
                formal_slots.push(slot);
                continue;
            }
            let source_name = parameter_identities
                .as_ref()
                .and_then(|identities| identities.get(parameter as usize))
                .and_then(|identity| identity.source_name.clone());
            match (self.ir.expr(value), source_name) {
                (IrExpr::GetValue(slot), None) => formal_slots.push(*slot),
                (_, name) => {
                    let slot = self.allocate_temporary();
                    let declaration = self.ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty,
                        init: Some(value),
                        named: name.is_some(),
                    });
                    self.ir.call_operand_bindings.insert(declaration);
                    if let Some(name) = name {
                        self.ir.value_names.insert(declaration, name);
                        self.ir.set_debug_local_provenance(
                            declaration,
                            crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { depth: 0 },
                        );
                    }
                    preamble.push(declaration);
                    formal_slots.push(slot);
                }
            }
        }
        let marker = self.inline_marker(
            frame.callee.to_string(),
            crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
                implementation,
                depth: 0,
            },
        );
        if let Some(line) = call_line {
            self.ir.expr_lines.insert(marker, line);
            if frame.inline_only && first_body_line(self.ir, inline_body) == Some(line) {
                self.ir.inline_synthetic_lines.insert(marker);
            }
        }
        preamble.push(marker);

        let local_base = self.next_temporary;
        let local_count =
            rehome_inline_body_values(self.ir, inline_body, &formal_slots, local_base)?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.functions[implementation as usize].body = None;
        self.ir.inline_only_fns.insert(implementation);
        // A `finally` protects the lambda parameters and its inline-frame marker, so they open the
        // spliced body. A call without one keeps them as statements ahead of the body; that is
        // where an inlined `return this` closes the frame before the read.
        let inline_body = if preamble_in_body {
            self.prepend_lambda_preamble(inline_body, preamble)
        } else {
            statements.extend(preamble);
            inline_body
        };
        Some((inline_body, callee_receiver))
    }

    /// The lambda parameter bindings and `$i$a$` marker open the spliced body, so they sit inside
    /// the call's protected region rather than ahead of it.
    fn prepend_lambda_preamble(&mut self, body: ExprId, preamble: Vec<ExprId>) -> ExprId {
        if preamble.is_empty() {
            return body;
        }
        match self.ir.expr(body).clone() {
            IrExpr::Block { mut stmts, value } => {
                let mut combined = preamble;
                combined.append(&mut stmts);
                self.ir.exprs[body as usize] = IrExpr::Block {
                    stmts: combined,
                    value,
                };
                body
            }
            _ => {
                let wrapped = self.ir.add_expr(IrExpr::Block {
                    stmts: preamble,
                    value: Some(body),
                });
                if let Some(&end) = self.ir.expr_end_lines.get(&body) {
                    self.ir.expr_end_lines.insert(wrapped, end);
                }
                if let Some(&line) = self.ir.expr_source_lines.get(&body) {
                    self.ir.expr_source_lines.insert(wrapped, line);
                }
                if let Some(&ty) = self.ir.logical_types.get(&body) {
                    self.ir.logical_types.insert(wrapped, ty);
                }
                wrapped
            }
        }
    }

    /// Invoke an already-evaluated function value when it has no local inline template to splice.
    ///
    /// A caller parameter is normally a `GetValue` whose declaration lives outside the call's
    /// materialization statements. That absence says only that the body is not locally available;
    /// it must not make a complete provider-owned inline plan fall back to an external stdlib call.
    fn invoke_external_function_value(
        &mut self,
        function: ExprId,
        lambda_ty: Ty,
        invocation_operands: &[(ExprId, Ty)],
    ) -> Option<ExprId> {
        let Ty::Fun(signature) = lambda_ty else {
            return None;
        };
        if invocation_operands.len() != signature.params.len() {
            return None;
        }
        Some(
            self.ir.add_expr(IrExpr::InvokeFunction {
                func: function,
                args: invocation_operands
                    .iter()
                    .map(|(operand, _)| *operand)
                    .collect(),
                params: signature.params.to_vec(),
                ret: signature.ret,
            }),
        )
    }
}
