use crate::fir::{
    FirBinaryOperation, FirConstant, FirConversion, FirConversionKind, FirExprId, FirExprKind,
    FirIndexedAccessKind, FirJumpKind, FirReceiver, FirTypeOperation, FirUnaryOperation,
    ResolvedTy,
};
use crate::ir::{Callee, ExprId, IrBinOp, IrCatch, IrConst, IrExpr, IrIntrinsic, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure, LoweringState};

/// The unoptimized lowering dispatcher reserves frames of the same order as the checker's. Check
/// before the first frame and then every 64 nested frames: that keeps the largest unchecked run
/// well below one grown segment without paying `stacker`'s probe for every ordinary node.
const EXPRESSION_STACK_CHECK_INTERVAL: u32 = 64;

impl BodyLowering<'_> {
    /// Lower one expression, growing the stack when this recursion is about to outrun the current
    /// segment.
    ///
    /// The checker bounds semantic nesting, but the bound is only survivable if the stack lasts
    /// long enough to reach it, and lowering recurses over the same nesting with frames of its own.
    /// The lowerer this pass replaced entered on a grown segment and re-checked per level for
    /// exactly this reason; without it a deeply nested call chain exhausts a small embedder stack
    /// and dies on the guard page instead of degrading.
    pub(super) fn expression(
        &mut self,
        expression_id: FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        self.expression_depth = self
            .expression_depth
            .checked_add(1)
            .expect("expression nesting exceeds u32");
        let check_stack = self.expression_depth == 1
            || self
                .expression_depth
                .is_multiple_of(EXPRESSION_STACK_CHECK_INTERVAL);
        let result = if check_stack {
            crate::wide_stack::on_wide_stack(|| self.expression_inner(expression_id))
        } else {
            self.expression_inner(expression_id)
        };
        self.expression_depth -= 1;
        result
    }

    fn expression_inner(&mut self, expression_id: FirExprId) -> Result<ExprId, FirLoweringFailure> {
        match self.expression_state(expression_id) {
            Some(LoweringState::Lowered(expression)) => return Ok(expression),
            Some(LoweringState::Computing) => {
                return Err(FirLoweringFailure::RecursiveExpression(expression_id));
            }
            Some(LoweringState::Uncomputed) => {}
            None => return Err(FirLoweringFailure::MissingExpression(expression_id)),
        }
        self.set_expression_state(expression_id, LoweringState::Computing);
        let expression = self
            .body
            .expr(expression_id)
            .ok_or(FirLoweringFailure::MissingExpression(expression_id))?;
        let origin = expression.origin;
        let first_generated = self.ir.exprs.len();
        // kotlinc folds an intrinsic-const operation over constants before any other lowering, so
        // the whole operation becomes its value.
        let folded = self.constants.fold(self.body, expression_id);
        // kotlinc then flattens nested concatenations into one, merging each run of constant
        // arguments into one `String` constant.
        let concatenation = folded
            .is_none()
            .then(|| super::string_concatenation::flattened_concatenation(self.body, expression_id))
            .flatten();
        let lowered = match &expression.kind {
            _ if concatenation.is_some() => {
                let parts = concatenation.expect("guarded by the arm");
                let runs = self.constants.template_runs(self.body, &parts);
                let parts = runs
                    .iter()
                    .map(|run| match run {
                        super::constant_evaluation::TemplateRun::Constant { value, source } => {
                            let text = lower_constant(value, Ty::String, origin)?;
                            let lowered = self.ir.add_expr(IrExpr::Const(text));
                            // The folded literal is marked where its first constant was written.
                            // A later constant absorbed into the same run does not open a line.
                            let debug = self.body.expression_debug_lines(*source);
                            if debug.positionless {
                                self.ir.mark_positionless(lowered);
                            }
                            if debug.source != 0 {
                                self.ir.expr_source_lines.insert(lowered, debug.source);
                            }
                            if debug.end != 0 {
                                self.ir.expr_end_lines.insert(lowered, debug.end);
                            }
                            Ok((lowered, None))
                        }
                        super::constant_evaluation::TemplateRun::Part(part) => Ok((
                            self.expression_with_conversion(part.value, part.conversion)?,
                            Some(self.converted_type(part.value, part.conversion)?),
                        )),
                    })
                    .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
                // Like a call's operands, a concatenation's parts evaluate into source-order locals
                // once one of them suspends, so the suspension can split the body between them.
                if parts.iter().any(|&(part, _)| self.operand_suspends(part)) {
                    let mut statements = Vec::new();
                    let parts = parts
                        .into_iter()
                        .map(|(part, ty)| match ty {
                            Some(ty) => self.spill_call_operand(part, ty, &mut statements),
                            None => part,
                        })
                        .collect();
                    let value = self.ir.add_expr(IrExpr::StringConcat(parts));
                    self.ir.add_expr(IrExpr::Block {
                        stmts: statements,
                        value: Some(value),
                    })
                } else {
                    let parts = parts.into_iter().map(|(part, _)| part).collect();
                    self.ir.add_expr(IrExpr::StringConcat(parts))
                }
            }
            _ if folded.is_some() => {
                let folded = folded.expect("guarded by the arm");
                let constant = lower_constant(&folded.value, expression.ty.get(), origin)?;
                self.ir.add_expr(IrExpr::Const(constant))
            }
            FirExprKind::Constant(constant) => {
                let constant = lower_constant(constant, expression.ty.get(), origin)?;
                self.ir.add_expr(IrExpr::Const(constant))
            }
            FirExprKind::ForwardedSuperArgument { slot } => self
                .ir
                .add_expr(IrExpr::ForwardedSuperArgument { slot: *slot }),
            FirExprKind::ArrayLiteral {
                array_type,
                elements,
            } => self.array_literal(*array_type, elements)?,
            FirExprKind::ArrayConstruction {
                array_type,
                element_type,
                size,
                size_conversion,
                initializer,
            } => self.array_construction(
                *array_type,
                *element_type,
                *size,
                *size_conversion,
                *initializer,
            )?,
            FirExprKind::ImplicitReceiver { current, depth } => {
                let slot = self
                    .implicit_receiver_slot(*current, *depth)
                    .ok_or(FirLoweringFailure::MissingImplicitReceiver { origin })?;
                self.ir.add_expr(IrExpr::GetValue(slot))
            }
            FirExprKind::EnclosingReceiver { path } => self.enclosing_receiver(path, origin)?,
            FirExprKind::CapturedImplicitReceiver {
                enclosing_depth,
                current,
                depth,
                path,
            } => {
                let slot = self
                    .implicit_receiver_capture_slot(*enclosing_depth, *current, *depth, path)
                    .ok_or(FirLoweringFailure::MissingImplicitReceiver { origin })?;
                self.ir.add_expr(IrExpr::GetValue(slot))
            }
            FirExprKind::SingletonValue { classifier } => self.checked_singleton(*classifier),
            FirExprKind::EnumEntry {
                classifier,
                ordinal: _,
                name,
            } => self.ir.add_expr(IrExpr::EnumEntry {
                classifier: *classifier,
                name: name.clone(),
            }),
            FirExprKind::ClassifierPropertyRead { owner, property } => match property {
                crate::fir::FirClassifierProperty::EnumEntries => {
                    self.ir.add_expr(IrExpr::EnumEntries { classifier: *owner })
                }
            },
            FirExprKind::ValueRead(value) => {
                if let Some(element) = self.shared_local_type(*value) {
                    self.shared_cell_read(self.value_slot(*value), element)
                } else {
                    let read = self.ir.add_expr(IrExpr::GetValue(self.value_slot(*value)));
                    if let Some(stability) = self.binding_stability.get(value).copied() {
                        self.ir.binding_read_stability.insert(read, stability);
                    }
                    read
                }
            }
            FirExprKind::LateinitRead { value, name } => {
                let operand = self.expression(*value)?;
                self.ir.add_expr(IrExpr::LateinitCheck {
                    operand,
                    name: name.to_string(),
                })
            }
            FirExprKind::ValueWrite {
                target,
                value,
                conversion,
            } => {
                let value = self.expression_with_conversion(*value, *conversion)?;
                if let Some(element) = self.shared_local_type(*target) {
                    self.shared_cell_write(self.value_slot(*target), element, value)
                } else {
                    self.ir.add_expr(IrExpr::SetValue {
                        var: self.value_slot(*target),
                        value,
                    })
                }
            }
            FirExprKind::PropertyRead {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                substitutions,
            } => {
                let lowered = self.checked_property_read(
                    target,
                    *dispatch_receiver,
                    *extension_receiver,
                    context_arguments,
                    substitutions,
                )?;
                let declared = match target {
                    crate::fir::FirPropertyTarget::Module { property, .. } => self
                        .index
                        .property_declaration(*property)
                        .and_then(|declaration| self.index.signature(declaration))
                        .map(|signature| signature.result.get()),
                    crate::fir::FirPropertyTarget::External { result, .. } => Some(result.get()),
                };
                if declared.is_some_and(|declared| declared != expression.ty.get()) {
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: lowered,
                        type_operand: expression.ty.get(),
                    })
                } else {
                    lowered
                }
            }
            FirExprKind::PropertyWrite {
                target,
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                conversion,
                substitutions,
            } => self.checked_property_write(
                target,
                *dispatch_receiver,
                *extension_receiver,
                context_arguments,
                *value,
                *conversion,
                substitutions,
            )?,
            FirExprKind::LateinitFieldRead {
                target,
                dispatch_receiver,
            } => {
                let dispatch_receiver = self.receiver(*dispatch_receiver)?;
                self.ir.add_expr(IrExpr::Checked(
                    crate::ir::IrCheckedOperation::LateinitFieldRead {
                        target: *target,
                        dispatch_receiver,
                    },
                ))
            }
            FirExprKind::BackingFieldRead {
                target,
                dispatch_receiver,
            } => {
                let dispatch_receiver = self.receiver(*dispatch_receiver)?;
                self.ir.add_expr(IrExpr::Checked(
                    crate::ir::IrCheckedOperation::BackingFieldRead {
                        target: *target,
                        dispatch_receiver,
                    },
                ))
            }
            FirExprKind::BackingFieldWrite {
                target,
                dispatch_receiver,
                value,
                conversion,
            } => {
                let dispatch_receiver = self.receiver(*dispatch_receiver)?;
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::Checked(
                    crate::ir::IrCheckedOperation::BackingFieldWrite {
                        target: *target,
                        dispatch_receiver,
                        value,
                    },
                ))
            }
            FirExprKind::PluginExpression {
                plugin,
                operation,
                data,
                types,
                operands,
            } => {
                let operands = operands
                    .iter()
                    .map(|operand| {
                        self.expression_with_conversion(operand.value, operand.conversion)
                    })
                    .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
                self.ir.add_expr(IrExpr::PluginPlaceholder {
                    plugin,
                    kind: operation,
                    exprs: operands,
                    data: data.to_vec(),
                    types: types.iter().map(|ty| ty.get()).collect(),
                })
            }
            FirExprKind::Call(call) => {
                let lowered = self
                    .checked_call(call, self.body.expression_debug_lines(expression_id).source)?;
                let declared_result = match call.target {
                    crate::fir::FirCallTarget::Module(target) => self
                        .index
                        .callable(target)
                        .and_then(|callable| self.index.signature(callable.declaration))
                        .map(|signature| signature.result.get()),
                    crate::fir::FirCallTarget::External { result, .. }
                    | crate::fir::FirCallTarget::Intrinsic { result, .. }
                    | crate::fir::FirCallTarget::Classifier { result, .. } => Some(result.get()),
                    // The PHYSICAL result is what the emitted call leaves on the stack; a generic
                    // supertype declaration returns its erasure, so the coercion to the logical
                    // result has to be an explicit IR node.
                    crate::fir::FirCallTarget::Super {
                        ref physical_result,
                        ..
                    } => Some(physical_result.get()),
                };
                self.declaration_result(lowered, declared_result, expression.ty.get())
            }
            FirExprKind::ConstructorCall(call) => self.checked_constructor_call(call)?,
            FirExprKind::AnonymousObject(object) => self.anonymous_object(object)?,
            FirExprKind::ComparisonCall { operation, call } => {
                let call = self
                    .checked_call(call, self.body.expression_debug_lines(expression_id).source)?;
                if let IrExpr::Call {
                    callee:
                        Callee::Intrinsic {
                            operation:
                                IrIntrinsic::PrimitiveCompare {
                                    relational_operator,
                                    ..
                                },
                            ..
                        },
                    ..
                } = &mut self.ir.exprs[call as usize]
                {
                    *relational_operator = true;
                }
                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: lower_binary_operation(*operation),
                    lhs: call,
                    rhs: zero,
                })
            }
            FirExprKind::ContainmentCall { call, negated } => {
                let call = self
                    .checked_call(call, self.body.expression_debug_lines(expression_id).source)?;
                if *negated {
                    self.ir.add_negation(call)
                } else {
                    call
                }
            }
            FirExprKind::ClassLiteral { classifier, value } => {
                self.checked_class_literal(*classifier, *value)?
            }
            FirExprKind::CallableReference {
                target,
                function_type,
                reflective,
                binding,
                dispatch_receiver,
                extension_receiver,
                substitutions,
                adaptation,
                reflection_owner,
            } => self.checked_callable_reference(super::checked::CheckedCallableReference {
                target: target.clone(),
                binding: *binding,
                dispatch_receiver: *dispatch_receiver,
                extension_receiver: *extension_receiver,
                substitutions,
                adaptation: adaptation.as_deref(),
                reference_ty: function_type.get(),
                reflective: *reflective,
                reflection_owner: *reflection_owner,
            })?,
            FirExprKind::LocalCallableReference {
                target,
                function_type,
                reflective: _,
                extension_receiver,
                adaptation,
            } => self.checked_local_callable_reference(
                target.clone(),
                *extension_receiver,
                adaptation.as_deref(),
                function_type.get(),
            )?,
            FirExprKind::LocalPropertyReference {
                name,
                property_type,
                declaration,
                mutable,
                ordinal,
            } => {
                let reference = self.local_property_reference(
                    *declaration,
                    (name, property_type.get()),
                    (*mutable, *ordinal),
                )?;
                self.ir.add_expr(IrExpr::LocalPropertyReference(reference))
            }
            FirExprKind::LocalDelegateAccess {
                plan,
                delegate,
                dispatch_receiver,
                value,
            } => {
                let plan = self.ir.local_delegate_plan_ids.get(plan).copied().ok_or(
                    FirLoweringFailure::InvalidLocalDelegatePlan(self.body.owner()),
                )?;
                let delegate = self.expression(*delegate)?;
                let dispatch_receiver = self.receiver(*dispatch_receiver)?;
                let value = value.map(|value| self.expression(value)).transpose()?;
                self.ir.add_expr(IrExpr::LocalDelegateAccess(
                    crate::ir::IrLocalDelegateAccess {
                        plan,
                        delegate,
                        dispatch_receiver,
                        value,
                    },
                ))
            }
            FirExprKind::PropertyReference {
                target,
                function_type,
                reflective,
                binding,
                dispatch_receiver,
                extension_receiver,
                mutable,
                substitutions,
                adaptation,
            } => self.checked_property_reference(
                target,
                *binding,
                *dispatch_receiver,
                *extension_receiver,
                *mutable,
                substitutions,
                adaptation.as_deref(),
                function_type.get(),
                *reflective,
            )?,
            FirExprKind::TypeOperation {
                operation,
                operand,
                target,
            } => {
                let operand_id = *operand;
                let operand_ty = self
                    .body
                    .expr(operand_id)
                    .ok_or(FirLoweringFailure::MissingExpression(operand_id))?
                    .ty
                    .get();
                let operand = self.expression(operand_id)?;
                let operand =
                    if operand_ty == Ty::Unit && !matches!(operation, FirTypeOperation::SafeCast) {
                        self.unit_value_after_effect(operand)
                    } else {
                        operand
                    };
                match operation {
                    FirTypeOperation::NotNullAssertion => {
                        let asserted = self.ir.add_expr(IrExpr::NotNullAssert {
                            operand,
                            check: crate::ir::NullCheck::Source,
                        });
                        if operand_ty != target.get() {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: asserted,
                                type_operand: target.get(),
                            })
                        } else {
                            asserted
                        }
                    }
                    FirTypeOperation::SafeCast => {
                        self.safe_cast_expression(operand_id, target.get())?
                    }
                    FirTypeOperation::Is | FirTypeOperation::NotIs
                        if target.get().is_nullable() =>
                    {
                        // JVM `instanceof` is false for null, while Kotlin's `x is T?` is true.
                        // Evaluate the checked operand once, retain it as a language-level reference,
                        // and expand the nullable test without resolving or reinterpreting its type.
                        let operand_ty = self
                            .body
                            .expr(operand_id)
                            .ok_or(FirLoweringFailure::MissingExpression(operand_id))?
                            .ty
                            .get();
                        let reference = if operand_ty.is_reference() {
                            operand
                        } else {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: operand,
                                type_operand: crate::types::Ty::nullable(crate::types::Ty::obj(
                                    "kotlin/Any",
                                )),
                            })
                        };
                        let temporary = self.allocate_temporary();
                        let variable = self.ir.add_expr(IrExpr::Variable {
                            index: temporary,
                            ty: crate::types::Ty::nullable(crate::types::Ty::obj("kotlin/Any")),
                            init: Some(reference),
                            named: false,
                        });
                        let nullable_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                        let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                        let negated = *operation == FirTypeOperation::NotIs;
                        let null_test = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: if negated {
                                IrBinOp::RefNe
                            } else {
                                IrBinOp::RefEq
                            },
                            lhs: nullable_read,
                            rhs: null,
                        });
                        let instance_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                        let instance_test =
                            self.instance_check(negated, instance_read, target.get().non_null());
                        let combined = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: if negated { IrBinOp::And } else { IrBinOp::Or },
                            lhs: null_test,
                            rhs: instance_test,
                        });
                        self.ir.add_expr(IrExpr::Block {
                            stmts: vec![variable],
                            value: Some(combined),
                        })
                    }
                    FirTypeOperation::Is | FirTypeOperation::NotIs => self.instance_check(
                        *operation == FirTypeOperation::NotIs,
                        operand,
                        target.get(),
                    ),
                    FirTypeOperation::Cast => {
                        let cast = self.ir.add_expr(IrExpr::TypeOp {
                            op: lower_type_operation(*operation, target.get()),
                            arg: operand,
                            type_operand: target.get(),
                        });
                        self.ir.written_casts.insert(cast);
                        cast
                    }
                }
            }
            FirExprKind::ImplicitConversion { value, conversion } => {
                self.expression_with_conversion(*value, Some(*conversion))?
            }
            FirExprKind::Unary { operation, operand } => {
                let operand = self.expression(*operand)?;
                match operation {
                    FirUnaryOperation::Negate => {
                        if let IrExpr::Const(constant) = self.ir.expr(operand) {
                            if let Some(constant) = super::constant_folding::negate(constant) {
                                self.ir.add_expr(IrExpr::Const(constant))
                            } else {
                                self.ir.add_expr(IrExpr::PrimitiveNeg {
                                    operand,
                                    ty: expression.ty.get(),
                                })
                            }
                        } else {
                            self.ir.add_expr(IrExpr::PrimitiveNeg {
                                operand,
                                ty: expression.ty.get(),
                            })
                        }
                    }
                    FirUnaryOperation::Identity => operand,
                    FirUnaryOperation::BooleanNot => self.ir.add_negation(operand),
                    FirUnaryOperation::Increment | FirUnaryOperation::Decrement => {
                        // kotlinc's `inc`/`dec` intrinsic adds a signed delta: `dec` is `+ -1`.
                        let result = expression.ty.get();
                        let delta: i8 = match operation {
                            FirUnaryOperation::Increment => 1,
                            _ => -1,
                        };
                        let delta = self.ir.add_expr(IrExpr::Const(match result.non_null() {
                            crate::types::Ty::Long | crate::types::Ty::ULong => {
                                IrConst::Long(delta.into())
                            }
                            crate::types::Ty::Float => IrConst::Float(delta.into()),
                            crate::types::Ty::Double => IrConst::Double(delta.into()),
                            _ => IrConst::Int(delta.into()),
                        }));
                        let updated = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::Add,
                            lhs: operand,
                            rhs: delta,
                        });
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: updated,
                            type_operand: result,
                        })
                    }
                    FirUnaryOperation::BitwiseNot => {
                        // `inv` xors with every bit set in the carrier. `ULong` is that `long`
                        // carrier, the same width `Long.inv` already uses.
                        let carrier = expression.ty.get().canonical_semantic().non_null();
                        let all_bits = self.ir.add_expr(IrExpr::Const(
                            if matches!(carrier, crate::types::Ty::Long | crate::types::Ty::ULong) {
                                IrConst::Long(-1)
                            } else {
                                IrConst::Int(-1)
                            },
                        ));
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::BitXor,
                            lhs: operand,
                            rhs: all_bits,
                        })
                    }
                }
            }
            FirExprKind::Equality {
                operation,
                mode,
                lhs,
                rhs,
            } => {
                let lhs = self.expression(*lhs)?;
                let rhs = self.expression(*rhs)?;
                self.ir.add_expr(IrExpr::Equality {
                    op: lower_binary_operation(*operation),
                    mode: *mode,
                    lhs,
                    rhs,
                })
            }
            FirExprKind::Binary {
                operation,
                lhs,
                rhs,
            } => {
                let lhs = self.expression(*lhs)?;
                match operation {
                    FirBinaryOperation::BooleanAnd => {
                        let rhs = self.expression(*rhs)?;
                        self.short_circuit_and(lhs, rhs)
                    }
                    FirBinaryOperation::BooleanOr => {
                        let rhs = self.expression(*rhs)?;
                        self.short_circuit_or(lhs, rhs)
                    }
                    FirBinaryOperation::Add
                    | FirBinaryOperation::Subtract
                    | FirBinaryOperation::Multiply
                    | FirBinaryOperation::Divide
                    | FirBinaryOperation::Remainder
                    | FirBinaryOperation::Equal
                    | FirBinaryOperation::NotEqual
                    | FirBinaryOperation::Less
                    | FirBinaryOperation::LessOrEqual
                    | FirBinaryOperation::Greater
                    | FirBinaryOperation::GreaterOrEqual
                    | FirBinaryOperation::ReferentialEqual
                    | FirBinaryOperation::ReferentialNotEqual
                    | FirBinaryOperation::BitwiseAnd
                    | FirBinaryOperation::BitwiseOr
                    | FirBinaryOperation::BitwiseXor
                    | FirBinaryOperation::ShiftLeft
                    | FirBinaryOperation::ShiftRight
                    | FirBinaryOperation::UnsignedShiftRight => {
                        let rhs = self.expression(*rhs)?;
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: lower_binary_operation(*operation),
                            lhs,
                            rhs,
                        })
                    }
                }
            }
            FirExprKind::NullablePrimitiveComparison {
                operation,
                nullable,
                primitive,
                primitive_ty,
                nullable_first,
            } if super::floating_equality::is_floating(primitive_ty.get()) => {
                let (lhs, rhs) = if *nullable_first {
                    (*nullable, *primitive)
                } else {
                    (*primitive, *nullable)
                };
                self.floating_equality(expression_id, *operation, lhs, rhs, primitive_ty.get())?
            }
            FirExprKind::NullableNumericComparison {
                operation,
                lhs,
                rhs,
                lhs_primitive,
                rhs_primitive,
                comparison,
            } if super::floating_equality::is_floating(comparison.get())
                && lhs_primitive.get() == comparison.get()
                && rhs_primitive.get() == comparison.get() =>
            {
                self.floating_equality(expression_id, *operation, *lhs, *rhs, comparison.get())?
            }
            FirExprKind::NullablePrimitiveComparison {
                operation,
                nullable,
                primitive,
                primitive_ty,
                nullable_first: _,
            } => {
                let nullable_value = self.expression(*nullable)?;
                let temporary = self.allocate_temporary();
                let nullable_ty = self
                    .body
                    .expr(*nullable)
                    .ok_or(FirLoweringFailure::MissingExpression(*nullable))?
                    .ty
                    .get();
                let variable = self.ir.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: nullable_ty,
                    init: Some(nullable_value),
                    named: false,
                });
                let nullable_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let is_null = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: nullable_read,
                    rhs: null,
                });
                let nullable_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                let unboxed = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: nullable_read,
                    type_operand: primitive_ty.get(),
                });
                let primitive = self.expression(*primitive)?;
                let compared = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: lower_binary_operation(*operation),
                    lhs: unboxed,
                    rhs: primitive,
                });
                let fixed = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(
                    *operation == FirBinaryOperation::NotEqual,
                )));
                let comparison = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(is_null), fixed), (None, compared)],
                });
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![variable],
                    value: Some(comparison),
                })
            }
            FirExprKind::NullableNumericComparison {
                operation,
                lhs,
                rhs,
                lhs_primitive,
                rhs_primitive,
                comparison,
            } => {
                let lhs_value = self.expression(*lhs)?;
                let rhs_value = self.expression(*rhs)?;
                let lhs_temporary = self.allocate_temporary();
                let rhs_temporary = self.allocate_temporary();
                let lhs_ty = self
                    .body
                    .expr(*lhs)
                    .ok_or(FirLoweringFailure::MissingExpression(*lhs))?
                    .ty
                    .get();
                let rhs_ty = self
                    .body
                    .expr(*rhs)
                    .ok_or(FirLoweringFailure::MissingExpression(*rhs))?
                    .ty
                    .get();
                let lhs_variable = self.ir.add_expr(IrExpr::Variable {
                    index: lhs_temporary,
                    ty: lhs_ty,
                    init: Some(lhs_value),
                    named: false,
                });
                let rhs_variable = self.ir.add_expr(IrExpr::Variable {
                    index: rhs_temporary,
                    ty: rhs_ty,
                    init: Some(rhs_value),
                    named: false,
                });
                let null_test = |lowering: &mut Self, temporary| {
                    let value = lowering.ir.add_expr(IrExpr::GetValue(temporary));
                    let null = lowering.ir.add_expr(IrExpr::Const(IrConst::Null));
                    lowering.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::RefEq,
                        lhs: value,
                        rhs: null,
                    })
                };
                let converted = |lowering: &mut Self, temporary, primitive: ResolvedTy| {
                    let value = lowering.ir.add_expr(IrExpr::GetValue(temporary));
                    let unboxed = lowering.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: value,
                        type_operand: primitive.get(),
                    });
                    if primitive == *comparison {
                        unboxed
                    } else {
                        lowering.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: unboxed,
                            type_operand: comparison.get(),
                        })
                    }
                };
                let lhs_present = converted(self, lhs_temporary, *lhs_primitive);
                let rhs_present = converted(self, rhs_temporary, *rhs_primitive);
                let present_comparison = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: lower_binary_operation(*operation),
                    lhs: lhs_present,
                    rhs: rhs_present,
                });
                let rhs_is_null = null_test(self, rhs_temporary);
                let rhs_null_result = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(
                    *operation == FirBinaryOperation::NotEqual,
                )));
                let lhs_present_result = self.ir.add_expr(IrExpr::When {
                    branches: vec![
                        (Some(rhs_is_null), rhs_null_result),
                        (None, present_comparison),
                    ],
                });
                let lhs_is_null = null_test(self, lhs_temporary);
                let rhs_null_comparison = null_test(self, rhs_temporary);
                let lhs_null_result = if *operation == FirBinaryOperation::Equal {
                    rhs_null_comparison
                } else {
                    self.ir.add_negation(rhs_null_comparison)
                };
                let result = self.ir.add_expr(IrExpr::When {
                    branches: vec![
                        (Some(lhs_is_null), lhs_null_result),
                        (None, lhs_present_result),
                    ],
                });
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![lhs_variable, rhs_variable],
                    value: Some(result),
                })
            }
            FirExprKind::Range {
                operation,
                start,
                start_type,
                end,
                end_type,
            } => self.range_expression(
                *operation,
                *start,
                start_type.get(),
                *end,
                end_type.get(),
                expression.ty.get(),
            )?,
            FirExprKind::InRange {
                operation,
                provenance: _,
                comparison,
                value,
                start,
                end,
                negated,
            } => self.in_range_expression(
                *operation,
                comparison.get(),
                *value,
                *start,
                *end,
                *negated,
                origin,
            )?,
            FirExprKind::StringTemplate(_) => {
                unreachable!("a string template folds to a constant or is a concatenation")
            }
            FirExprKind::AnnotationArray(values) => {
                let elements = values
                    .iter()
                    .copied()
                    .map(|value| self.expression(value))
                    .collect::<Result<Vec<_>, _>>()?;
                self.ir.add_expr(IrExpr::Vararg {
                    array_type: expression.ty.get(),
                    spreads: vec![false; elements.len()],
                    elements,
                })
            }
            FirExprKind::FunctionInvoke {
                callee,
                context_arguments,
                arguments,
                parameter_types,
                result,
                suspend,
            } => {
                let callee = self.expression(*callee)?;
                let mut values = context_arguments
                    .iter()
                    .map(|receiver| {
                        self.expression_with_conversion(receiver.value, receiver.conversion)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                values.extend(self.call_argument_values(arguments)?);
                let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
                    func: callee,
                    args: values,
                    params: parameter_types
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect(),
                    ret: expression.ty.get(),
                });
                if *suspend {
                    self.ir.suspend_calls.insert(invoke, result.get());
                }
                invoke
            }
            FirExprKind::ExtensionFunctionBinding {
                receiver,
                callable,
                target_parameters,
                receiver_parameter,
                target_result,
                suspend,
            } => self.checked_extension_function_binding(
                *receiver,
                *callable,
                target_parameters,
                *receiver_parameter,
                *target_result,
                *suspend,
            )?,
            FirExprKind::FunctionInvokeReference {
                callee,
                target_parameters,
                target_result,
                target_suspend,
                reference_parameters,
                reference_result,
                suspend,
                reflective,
            } => {
                let lowered = self.checked_function_invoke_reference(
                    super::function_references::CheckedFunctionInvokeReference {
                        callee: *callee,
                        target_parameters,
                        target_result: *target_result,
                        target_suspend: *target_suspend,
                        reference_parameters,
                        reference_result: *reference_result,
                        suspend: *suspend,
                        reflective: *reflective,
                    },
                )?;
                if *reflective {
                    // The carrier class takes the source reference's name. The forwarding adapter
                    // belongs to this reference node; realization replaces that node.
                    self.record_generated_class_provenance(expression_id, lowered as usize);
                }
                lowered
            }
            FirExprKind::IndexedRead {
                kind,
                receiver,
                indices,
            } => {
                let receiver = self.expression(*receiver)?;
                let arguments = indices
                    .iter()
                    .map(|index| self.expression_with_conversion(index.value, index.conversion))
                    .collect::<Result<Vec<_>, _>>()?;
                let operation = match kind {
                    FirIndexedAccessKind::Array => IrIntrinsic::ArrayGet,
                    FirIndexedAccessKind::String => IrIntrinsic::StringGet,
                };
                self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation,
                        ret: expression.ty.get(),
                    },
                    dispatch_receiver: Some(receiver),
                    args: arguments,
                })
            }
            FirExprKind::IndexedWrite {
                receiver,
                indices,
                value,
                conversion,
            } => {
                let receiver = self.expression(*receiver)?;
                let mut arguments = indices
                    .iter()
                    .map(|index| self.expression_with_conversion(index.value, index.conversion))
                    .collect::<Result<Vec<_>, _>>()?;
                arguments.push(self.expression_with_conversion(*value, *conversion)?);
                self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: IrIntrinsic::ArraySet,
                        ret: expression.ty.get(),
                    },
                    dispatch_receiver: Some(receiver),
                    args: arguments,
                })
            }
            FirExprKind::SafeCall { receiver, selector } => {
                self.safe_call_expression(receiver, *selector, expression.ty.get())?
            }
            FirExprKind::Elvis { lhs, rhs } => {
                let lhs_value = self.expression(*lhs)?;
                let temporary = self.allocate_temporary();
                let variable = self.ir.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: self
                        .body
                        .expr(*lhs)
                        .ok_or(FirLoweringFailure::MissingExpression(*lhs))?
                        .ty
                        .get(),
                    init: Some(lhs_value),
                    named: false,
                });
                let condition_lhs = self.ir.add_expr(IrExpr::GetValue(temporary));
                let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: condition_lhs,
                    rhs: null,
                });
                let rhs = self.expression(*rhs)?;
                let rhs = self.coerce_result(rhs, expression.ty.get());
                let lhs = self.ir.add_expr(IrExpr::GetValue(temporary));
                let lhs = self.coerce_result(lhs, expression.ty.get());
                let result = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(condition), rhs), (None, lhs)],
                });
                // An elvis over a safe call is one more guard of that call's chain: its left
                // value's null check joins the chain's, and every `null` reaches the right side.
                let over_safe_call = matches!(
                    self.ir.expr(lhs_value),
                    IrExpr::Block { value: Some(guard), .. } if self.ir.null_guards.contains(guard)
                );
                if over_safe_call {
                    self.ir.null_guards.insert(result);
                    self.ir.elvis_safe_call_guards.insert(result);
                }
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![variable],
                    value: Some(result),
                })
            }
            FirExprKind::Throw(value) => {
                let operand = self.expression(*value)?;
                self.ir.add_expr(IrExpr::Throw { operand })
            }
            FirExprKind::Jump {
                kind,
                target,
                value,
            } => {
                let value = value.map(|value| self.expression(value)).transpose()?;
                match kind {
                    FirJumpKind::Return { target_depth } => {
                        let returned = self.ir.add_expr(IrExpr::Return(value));
                        self.ir
                            .checked_return_depths
                            .insert(returned, *target_depth);
                        returned
                    }
                    FirJumpKind::Break { target_depth } => self.ir.add_expr(IrExpr::Break {
                        label: Some(self.control_label(*target_depth, *target)?),
                    }),
                    FirJumpKind::Continue { target_depth } => self.ir.add_expr(IrExpr::Continue {
                        label: Some(self.control_label(*target_depth, *target)?),
                    }),
                }
            }
            FirExprKind::Conditional {
                condition,
                then_branch,
                then_conversion,
                else_branch,
                else_conversion,
                deeply_exhaustive,
            } => {
                let condition = self.expression(*condition)?;
                let then_branch =
                    self.expression_with_conversion(*then_branch, *then_conversion)?;
                let else_branch =
                    self.expression_with_conversion(*else_branch, *else_conversion)?;
                let conditional = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(condition), then_branch), (None, else_branch)],
                });
                // The checker decided whether fir2ir types this `if` by its checked result; a
                // `Unit` one is a statement, whatever its branches' values.
                if expression.ty.get() == Ty::Unit || !*deeply_exhaustive {
                    self.ir.whens.exhaustive.insert(conditional, Ty::Unit);
                }
                conditional
            }
            FirExprKind::Try {
                body,
                catches,
                finally,
            } => {
                let body = self.expression(*body)?;
                let catches = catches
                    .iter()
                    .map(|catch| {
                        let parameter_ty = catch.parameter_ty.get();
                        if parameter_ty.non_null().obj_internal().is_none() {
                            return Err(FirLoweringFailure::InvalidCatchType {
                                origin: catch.origin,
                            });
                        }
                        Ok(IrCatch {
                            var: self.value_slot(catch.parameter),
                            binding: self
                                .body
                                .debug_value_name(catch.parameter)
                                .map(|name| crate::ir::IrCatchBinding::source(name.to_owned())),
                            ty: parameter_ty,
                            body: self.expression(catch.body)?,
                            line: (catch.debug_line != 0).then_some(catch.debug_line),
                        })
                    })
                    .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
                let finally = finally
                    .map(|finally| self.expression(finally))
                    .transpose()?;
                self.ir.add_expr(IrExpr::Try {
                    body,
                    catches,
                    finally,
                    result: expression.ty.get(),
                })
            }
            FirExprKind::When {
                subject,
                branches,
                deeply_exhaustive,
            } => {
                let line = self.body.expression_debug_lines(expression_id).source;
                self.when_expression(
                    *subject,
                    branches,
                    expression.ty.get(),
                    *deeply_exhaustive,
                    line,
                )?
            }
            FirExprKind::Block { statements, result } => {
                let mut lowered_statements = Vec::new();
                for statement in statements.iter().copied() {
                    let lowered = self.statement(statement)?;
                    if matches!(
                        self.body
                            .statement(statement)
                            .map(|statement| &statement.kind),
                        Some(crate::fir::FirStatementKind::Destructure { .. })
                    ) {
                        let IrExpr::Block { stmts, value: None } = self.ir.expr(lowered) else {
                            return Err(FirLoweringFailure::MalformedDestructureLowering {
                                origin: self
                                    .body
                                    .statement(statement)
                                    .expect("a checked block statement must exist")
                                    .origin,
                            });
                        };
                        lowered_statements.extend(stmts.iter().copied());
                    } else {
                        lowered_statements.push(lowered);
                    }
                }
                let value = result
                    .map(|result| {
                        let lowered = self.expression(result)?;
                        // The parser represents a block's final expression separately from its
                        // statement list, but it is still a source statement for debug mapping.
                        // Carry the checked line fact onto the lowered root just as ordinary FIR
                        // statements do; emission only consumes this map.
                        let line = self.body.expression_debug_lines(result).source;
                        if line != 0 {
                            self.ir.expr_lines.insert(lowered, line);
                        }
                        Ok(lowered)
                    })
                    .transpose()?;
                let block = self.ir.add_expr(IrExpr::Block {
                    stmts: lowered_statements,
                    value,
                });
                if self.root_block == Some(expression_id) {
                    self.lowered_root_block = Some(block);
                }
                block
            }
            FirExprKind::CapturedValueRead {
                enclosing_depth,
                source,
            } => self.captured_value(*enclosing_depth, *source)?,
            FirExprKind::ClassStorageRead { owner, field } => {
                self.class_storage_read(*owner, *field)?
            }
            FirExprKind::ConstructorCaptureRead {
                owner,
                field,
                shared_cell,
                site,
            } => {
                let holder = self.constructor_capture_parameter(*owner, *field, *site)?;
                if *shared_cell {
                    self.ir.add_expr(IrExpr::RefGet {
                        elem: expression.ty.get(),
                        holder,
                    })
                } else {
                    holder
                }
            }
            FirExprKind::ConstructorContextRead { owner, parameter } => {
                self.constructor_context_parameter(*owner, *parameter)?
            }
            FirExprKind::ClassStorageSharedRead { owner, field } => {
                let holder = self.class_storage_read(*owner, *field)?;
                self.ir.add_expr(IrExpr::RefGet {
                    elem: expression.ty.get(),
                    holder,
                })
            }
            FirExprKind::ClassStorageSharedWrite {
                owner,
                enclosing_depth,
                field,
                element,
                value,
                conversion,
            } => {
                let holder = self.enclosing_class_storage_read(*owner, *enclosing_depth, *field)?;
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::RefSet {
                    elem: element.get(),
                    holder,
                    value,
                })
            }
            FirExprKind::ConstructorCaptureSharedWrite {
                owner,
                field,
                element,
                value,
                conversion,
                site,
            } => {
                // `site` says WHICH captured cell — the coordinate this change carries — and
                // `element` says what the cell holds, which the checker settled when it made the
                // node. Neither stands in for the other: recovering the element from the value
                // being written would read the RHS's type where the CELL's is meant, and the two
                // differ the moment a subtype is assigned into it.
                let holder = self.constructor_capture_parameter(*owner, *field, *site)?;
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::RefSet {
                    elem: element.get(),
                    holder,
                    value,
                })
            }
            FirExprKind::EnclosingClassStorageRead {
                owner,
                enclosing_depth,
                field,
                shared_cell,
            } => {
                let holder = self.enclosing_class_storage_read(*owner, *enclosing_depth, *field)?;
                if *shared_cell {
                    self.ir.add_expr(IrExpr::RefGet {
                        elem: expression.ty.get(),
                        holder,
                    })
                } else {
                    holder
                }
            }
            FirExprKind::CapturedClassStorageRead {
                owner,
                receiver,
                path,
                field,
                shared_cell,
            } => {
                let holder = self.captured_class_storage_holder(*owner, *receiver, path, *field)?;
                if *shared_cell {
                    self.ir.add_expr(IrExpr::RefGet {
                        elem: expression.ty.get(),
                        holder,
                    })
                } else {
                    holder
                }
            }
            FirExprKind::CapturedClassStorageSharedWrite {
                owner,
                receiver,
                path,
                field,
                element,
                value,
                conversion,
            } => {
                let holder = self.captured_class_storage_holder(*owner, *receiver, path, *field)?;
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::RefSet {
                    elem: element.get(),
                    holder,
                    value,
                })
            }
            FirExprKind::CapturedValueWrite {
                enclosing_depth,
                source,
                value,
                conversion,
            } => {
                let capture = self
                    .capture_slots
                    .get(&(
                        *enclosing_depth,
                        crate::fir::FirCaptureSource::Value(*source),
                    ))
                    .cloned()
                    .ok_or(FirLoweringFailure::MissingCapture {
                        enclosing_depth: *enclosing_depth,
                        source: crate::fir::FirCaptureSource::Value(*source),
                    })?;
                let value = self.expression_with_conversion(*value, *conversion)?;
                if capture.shared_cell {
                    self.shared_cell_write(capture.slot, capture.ty, value)
                } else {
                    return Err(FirLoweringFailure::UnsharedCaptureWrite {
                        origin,
                        enclosing_depth: *enclosing_depth,
                        source: *source,
                    });
                }
            }
            FirExprKind::LocalCall {
                target,
                extension_receiver,
                arguments,
            } => {
                let (call, declared) =
                    self.checked_local_call(target.clone(), *extension_receiver, arguments)?;
                self.declaration_result(call, Some(declared), expression.ty.get())
            }
            FirExprKind::Lambda {
                callable,
                type_parameters,
                body,
            } => {
                let suspend = matches!(
                    expression.ty.get().non_null(),
                    crate::types::Ty::Fun(signature) if signature.suspend
                );
                let unit_method = self.unit_method_lambda.take() == Some(expression_id);
                let lambda = self.checked_lambda(*callable, body, suspend, unit_method)?;
                self.record_generated_class_provenance(expression_id, lambda as usize);
                // Every lambda keeps its place in the naming walk, whether or not a target writes
                // a class for it: a spliced lambda's inline-depth marker is spelled after it.
                if let IrExpr::Lambda { impl_fn, .. } = self.ir.exprs[lambda as usize] {
                    let provenance = self.generated_class_name_provenance(expression_id);
                    if let Some(origin) = self.ir.lambda_origins.get_mut(&impl_fn) {
                        origin.class_provenance = provenance;
                    }
                    // The checker published the declarations. Lowering does not look them up by name.
                    // Class-strategy `toString()` reads them from the lambda class's metadata,
                    // including a non-suspend lambda. A suspend lambda's class reads the same record.
                    let recorded =
                        super::generics::type_parameters_by_identity(self.index, type_parameters);
                    self.ir.record_lambda_type_parameters(impl_fn, recorded);
                    self.ir.record_lambda_class_provenance(
                        impl_fn,
                        crate::ir::type_reflection::LambdaClassProvenance::SourceFunction,
                    );
                }
                lambda
            }
        };
        self.ir.logical_types.insert(lowered, expression.ty.get());
        let lowered = crate::ir::complete_bottom_value(self.ir, lowered, expression.ty.get());
        self.ir.logical_types.insert(lowered, expression.ty.get());
        // A lambda's own provenance went to its node above; the references in its body carry
        // theirs.
        if !matches!(expression.kind, FirExprKind::Lambda { .. }) {
            self.record_callable_reference_provenance(expression_id, first_generated);
        }
        let debug = self.body.expression_debug_lines(expression_id);
        if debug.positionless {
            self.ir.mark_positionless(lowered);
        }
        if debug.source != 0 {
            self.ir.expr_source_lines.insert(lowered, debug.source);
            // A call whose operands were stored first keeps the call's line on the call itself, which
            // kotlinc marks once those operands are evaluated.
            if let Some(&call) = self.operand_bound_calls.get(&lowered) {
                self.ir
                    .expr_source_lines
                    .entry(call)
                    .or_insert(debug.source);
            }
            for raw in first_generated..self.ir.exprs.len() {
                let raw = raw as u32;
                if !self.ir.suspend_calls.contains_key(&raw) {
                    continue;
                }
                self.ir.expr_source_lines.entry(raw).or_insert(debug.source);
            }
        }
        if debug.end != 0 {
            self.ir.expr_end_lines.insert(lowered, debug.end);
            for raw in first_generated..self.ir.exprs.len() {
                let raw = raw as u32;
                if !self.ir.suspend_calls.contains_key(&raw) {
                    continue;
                }
                self.ir.expr_end_lines.entry(raw).or_insert(debug.end);
            }
        }
        self.record_expression_origins(first_generated, lowered, origin);
        self.set_expression_state(expression_id, LoweringState::Lowered(lowered));
        Ok(lowered)
    }

    /// Attach the naming provenance of a source callable reference to the one reference node its
    /// lowering produced. A reference whose owner has no common-IR class keeps no provenance.
    fn record_callable_reference_provenance(
        &mut self,
        expression_id: crate::fir::FirExprId,
        first_generated: usize,
    ) {
        // A receiver that is itself a reference (`create2(create("D")::test)::test`) is lowered
        // first and already owns its node. This expression owns the reference that is still unnamed.
        let mut references = (first_generated..self.ir.exprs.len()).filter(|&raw| {
            let unnamed = !self
                .ir
                .callable_reference_provenance
                .contains_key(&(raw as u32));
            unnamed
                && matches!(
                    self.ir.exprs[raw],
                    crate::ir::IrExpr::CallableReference(_)
                        | crate::ir::IrExpr::Checked(
                            crate::ir::IrCheckedOperation::PropertyReference { .. }
                        )
                )
        });
        let (Some(reference), None) = (references.next(), references.next()) else {
            return;
        };
        self.record_generated_class_provenance(expression_id, reference);
    }

    /// Attach the naming provenance and enclosure of a node a target may realize as a class of its
    /// own (a callable reference, a suspend lambda) to the IR node `node` lowered from it.
    fn record_generated_class_provenance(
        &mut self,
        expression_id: crate::fir::FirExprId,
        node: usize,
    ) {
        if let Some(enclosure) = self.enclosure {
            self.ir
                .callable_reference_enclosures
                .insert(node as u32, enclosure);
        }
        if let Some(provenance) = self.generated_class_name_provenance(expression_id) {
            self.ir
                .callable_reference_provenance
                .insert(node as u32, provenance);
        }
    }

    /// The naming walk's position for the class the source node `expression_id` compiles to, in
    /// common-IR terms. `None` when the walk gave it none, or its owner has no identity here.
    fn generated_class_name_provenance(
        &self,
        expression_id: crate::fir::FirExprId,
    ) -> Option<crate::ir::IrLocalClassNameProvenance> {
        let provenance = self.body.generated_class_provenance(expression_id)?;
        let lexical_owner = match provenance.lexical_owner {
            Some(owner) => match self.ir.checked_classifier_classes.get(&owner) {
                Some(&class) => Some(crate::ir::IrLocalClassOwner::Class(class)),
                None => {
                    let header = self
                        .index
                        .classifier_header(owner)
                        .filter(|_| self.index.local_class_name_provenance(owner).is_none())?;
                    Some(crate::ir::IrLocalClassOwner::External(header.classifier))
                }
            },
            None => None,
        };
        let source = self
            .index
            .declaration_anchor(crate::fir::DeclarationId::from_raw(self.body.owner().raw()))?
            .source;
        let package = self.index.source_package(source)?;
        Some(crate::ir::IrLocalClassNameProvenance {
            source: crate::ir::IrModuleSource { source, package },
            lexical_owner,
            segments: provenance.segments.clone(),
            ordinal: provenance.ordinal,
            parents: provenance.parents.clone(),
        })
    }

    /// Preserve the checked semantic conversion from a declaration's result to its call-site
    /// substitution. Whether that conversion crosses a physical ABI boundary is target-owned; the
    /// JVM records its answer after generic erasure.
    fn declaration_result(&mut self, call: ExprId, declared: Option<Ty>, result: Ty) -> ExprId {
        if declared.is_none_or(|declared| declared == result) {
            return call;
        }
        let coercion = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: result,
        });
        self.ir.declaration_result_coercions.insert(coercion);
        coercion
    }

    pub(super) fn expression_with_conversion(
        &mut self,
        expression: FirExprId,
        conversion: Option<FirConversion>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let checked = self
            .body
            .expr(expression)
            .ok_or(FirLoweringFailure::MissingExpression(expression))?;
        let source_type = checked.ty.get();
        let origin = checked.origin;
        let value = expression;
        if let Some(FirConversionKind::Sam(sam)) = conversion.map(|conversion| conversion.kind) {
            let recorded = self.body.sam_conversion(sam);
            let projected = recorded.is_some_and(|conversion| {
                conversion
                    .contravariant_parameters
                    .iter()
                    .any(|parameter| *parameter)
            });
            let unit_method = recorded.is_some_and(|conversion| {
                !conversion.suspend && conversion.declared_result.get() == crate::types::Ty::Unit
            });
            let literal = self
                .body
                .expr(value)
                .is_some_and(|operand| matches!(operand.kind, FirExprKind::Lambda { .. }));
            // An `in`-projected SAM types the literal as a Kotlin function (`Unit` return) and
            // adapts that value. A direct SAM implements the void method itself.
            if unit_method && literal && !projected {
                self.unit_method_lambda = Some(value);
            }
        }
        if let Some(folded) = conversion
            .and_then(|conversion| self.constants.fold_conversion(self.body, value, conversion))
        {
            let first_generated = self.ir.exprs.len();
            let constant = lower_constant(&folded.value, folded.ty, origin)?;
            let lowered = self.ir.add_expr(IrExpr::Const(constant));
            self.ir.logical_types.insert(lowered, folded.ty);
            let debug = self.body.expression_debug_lines(value);
            if debug.positionless {
                self.ir.mark_positionless(lowered);
            }
            if debug.source != 0 {
                self.ir.expr_source_lines.insert(lowered, debug.source);
            }
            if debug.end != 0 {
                self.ir.expr_end_lines.insert(lowered, debug.end);
            }
            self.record_expression_origins(first_generated, lowered, origin);
            return Ok(lowered);
        }
        let expression = self.expression(value)?;
        let converted = self.lowered_with_conversion(expression, source_type, conversion)?;
        if conversion.is_some_and(|conversion| {
            matches!(conversion.kind, FirConversionKind::FunctionValue { .. })
        }) {
            // The checker named the class a function-value conversion compiles to on its value.
            if let Some(&line) = self.ir.expr_source_lines.get(&expression) {
                self.ir.expr_source_lines.insert(converted, line);
            }
            self.record_generated_class_provenance(value, converted as usize);
        }
        Ok(converted)
    }

    /// Apply a checked conversion to an operand already lowered from a value of `source_type`.
    pub(super) fn lowered_with_conversion(
        &mut self,
        expression: ExprId,
        source_type: crate::types::Ty,
        conversion: Option<FirConversion>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let Some(conversion) = conversion else {
            return Ok(expression);
        };
        let conversion_origin = conversion.origin;
        Ok(match conversion.kind {
            FirConversionKind::NumericWidening { to }
            | FirConversionKind::NumericConversion { to } => self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: expression,
                type_operand: to.get(),
            }),
            FirConversionKind::NullabilityWidening { .. }
                if source_type == crate::types::Ty::Unit =>
            {
                self.unit_value_after_effect(expression)
            }
            FirConversionKind::NullabilityWidening { to } => self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: expression,
                type_operand: to.get(),
            }),
            FirConversionKind::SmartCast { to } => self.ir.add_expr(IrExpr::TypeOp {
                op: if to.get().is_reference() {
                    IrTypeOp::Cast
                } else {
                    IrTypeOp::ImplicitCoercion
                },
                arg: expression,
                type_operand: to.get(),
            }),
            FirConversionKind::PlatformNarrowing { narrowing, to } => {
                let check = self
                    .body
                    .platform_narrowing(narrowing)
                    .ok_or(FirLoweringFailure::UnsupportedConversion {
                        origin: conversion_origin,
                    })
                    .map(platform_null_check)?;
                if check == crate::ir::NullCheck::Unnamed {
                    return Ok(self.unnamed_null_check(expression, source_type, to.get()));
                }
                // A generic external call can already carry its checked result cast. Kotlin checks
                // the value produced by the call and then casts that checked value; keep the
                // frontend-selected assertion underneath that mechanical carrier conversion.
                let cast_operand = match self.ir.exprs.get(expression as usize) {
                    Some(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg,
                        ..
                    }) => Some(*arg),
                    _ => None,
                };
                if let Some(operand) = cast_operand {
                    let asserted = self.ir.add_expr(IrExpr::NotNullAssert { operand, check });
                    if let IrExpr::TypeOp { arg, .. } = &mut self.ir.exprs[expression as usize] {
                        *arg = asserted;
                    }
                    expression
                } else {
                    let asserted = self.ir.add_expr(IrExpr::NotNullAssert {
                        operand: expression,
                        check,
                    });
                    if source_type != to.get() {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: asserted,
                            type_operand: to.get(),
                        })
                    } else {
                        asserted
                    }
                }
            }
            FirConversionKind::Sam(sam) => {
                let conversion = self
                    .body
                    .sam_conversion(sam)
                    .ok_or(FirLoweringFailure::UnsupportedConversion {
                        origin: conversion.origin,
                    })?
                    .clone();
                let project_adapter = conversion
                    .contravariant_parameters
                    .iter()
                    .any(|parameter| *parameter);
                if !project_adapter
                    && matches!(
                        self.ir.exprs.get(expression as usize),
                        Some(IrExpr::Lambda { .. })
                    )
                {
                    let IrExpr::Lambda { impl_fn, sam, .. } =
                        &mut self.ir.exprs[expression as usize]
                    else {
                        unreachable!("the operand was just matched as a lambda");
                    };
                    let target = crate::ir::IrSamTarget {
                        classifier: conversion.classifier,
                        method: conversion.method.into(),
                        method_target: super::sam_conversions::ir_sam_method(
                            conversion.method_target,
                        ),
                        parameters: conversion.parameters.iter().map(|ty| ty.get()).collect(),
                        contravariant_parameters: conversion.contravariant_parameters.to_vec(),
                        result: conversion.result.get(),
                        declared_parameters: conversion
                            .declared_parameters
                            .iter()
                            .map(|ty| ty.get())
                            .collect(),
                        declared_result: conversion.declared_result.get(),
                        context_count: conversion.context_count,
                        has_receiver: conversion.has_receiver,
                        suspend: conversion.suspend,
                        source_suspend: conversion.source_suspend,
                        overridden_results: conversion
                            .overridden_results
                            .iter()
                            .map(|ty| ty.get())
                            .collect(),
                        function_adapter: false,
                        wraps_function_value: false,
                        // The lambda object is created here. A null check applies to a captured
                        // function value, not to this literal.
                        nullable: false,
                        kotlin_interface: conversion.kotlin_interface,
                        parameter_identities: conversion.parameter_identities.to_vec(),
                    };
                    self.ir.lambda_sam_signature.insert(
                        *impl_fn,
                        (target.declared_parameters.clone(), target.declared_result),
                    );
                    // A lambda literal checked directly against a suspend functional interface
                    // owns the interface method's suspend shape even when its source body contains
                    // no suspension point. Its pre-conversion expression type may be an ordinary
                    // function type, so `checked_lambda` cannot infer this from the child node. The
                    // selected SAM conversion is the authoritative semantic fact; publish the
                    // implementation to the backend suspend pass before attaching the target.
                    if target.suspend && !self.ir.suspend_funs.contains(impl_fn) {
                        self.ir.suspend_funs.push(*impl_fn);
                    }
                    assert!(sam.replace(target).is_none(), "a lambda has one SAM target");
                    expression
                } else {
                    self.sam_function_value_adapter(&conversion, expression)
                        .ok_or(FirLoweringFailure::UnsupportedConversion {
                            origin: conversion_origin,
                        })?
                }
            }
            FirConversionKind::FunctionValue { from, to, ordinal } => self
                .function_value_conversion(from, to, ordinal, expression)
                .ok_or(FirLoweringFailure::UnsupportedConversion {
                    origin: conversion_origin,
                })?,
            FirConversionKind::CoerceToUnit => self.unit_value_after_effect(expression),
        })
    }

    /// kotlinc's implicit not-null cast over a value it cannot name keeps the value in a temporary,
    /// checks the temporary, and yields it (`astore; aload; checkNotNull; aload`).
    fn unnamed_null_check(&mut self, value: ExprId, source_type: Ty, target: Ty) -> ExprId {
        let temporary = self.allocate_temporary();
        let stored = self.ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty: source_type,
            init: Some(value),
            named: false,
        });
        let checked = self.ir.add_expr(IrExpr::GetValue(temporary));
        let check = self.ir.add_expr(IrExpr::NotNullAssert {
            operand: checked,
            check: crate::ir::NullCheck::Unnamed,
        });
        let mut result = self.ir.add_expr(IrExpr::GetValue(temporary));
        if source_type != target {
            result = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: result,
                type_operand: target,
            });
        }
        self.ir.add_expr(IrExpr::Block {
            stmts: vec![stored, check],
            value: Some(result),
        })
    }

    pub(super) fn short_circuit_and(&mut self, lhs: ExprId, rhs: ExprId) -> ExprId {
        let false_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        let when = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(lhs), rhs), (None, false_value)],
        });
        self.ir
            .short_circuits
            .insert(when, crate::ir::IrShortCircuitKind::And);
        when
    }

    pub(super) fn short_circuit_or(&mut self, lhs: ExprId, rhs: ExprId) -> ExprId {
        let true_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let when = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(lhs), true_value), (None, rhs)],
        });
        self.ir
            .short_circuits
            .insert(when, crate::ir::IrShortCircuitKind::Or);
        when
    }

    /// Realize a checked boundary at which semantic `Unit` becomes a first-class value. Common IR
    /// retains the source effect and names the language-level singleton; target backends decide how
    /// that singleton and a statement-like Unit result are represented physically.
    fn unit_value_after_effect(&mut self, effect: ExprId) -> ExprId {
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        self.ir.add_expr(IrExpr::Block {
            stmts: vec![effect],
            value: Some(unit),
        })
    }

    fn call_argument_values(
        &mut self,
        arguments: &[crate::fir::FirCallArgument],
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        let mut values = Vec::new();
        for argument in arguments {
            match argument {
                crate::fir::FirCallArgument::Expression {
                    value, conversion, ..
                } => values.push(self.expression_with_conversion(*value, *conversion)?),
                crate::fir::FirCallArgument::Default { origin, .. }
                | crate::fir::FirCallArgument::Vararg { origin, .. } => {
                    return Err(FirLoweringFailure::UnsupportedConversion { origin: *origin });
                }
            }
        }
        Ok(values)
    }

    fn coerce_result(&mut self, expression: ExprId, target: crate::types::Ty) -> ExprId {
        if target == crate::types::Ty::Unit {
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            return self.ir.add_expr(IrExpr::Block {
                stmts: vec![expression],
                value: Some(unit),
            });
        }
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: expression,
            type_operand: target,
        })
    }

    fn safe_call_expression(
        &mut self,
        receiver: &FirReceiver,
        selector: FirExprId,
        result_type: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let receiver_value =
            self.expression_with_conversion(receiver.value, receiver.conversion)?;
        let receiver_type = self
            .body
            .expr(receiver.value)
            .ok_or(FirLoweringFailure::MissingExpression(receiver.value))?
            .ty
            .get();
        let temporary = self.allocate_temporary();
        let variable = self.ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty: receiver_type,
            init: Some(receiver_value),
            named: false,
        });
        // The selector executes only on the non-null branch. Publish that data-flow fact as an
        // explicit conversion so nullable primitive receivers are unboxed before their selected
        // operation; a raw nullable-slot read would put a wrapper into a primitive local.
        let selector_read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let selector_read = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: selector_read,
            type_operand: receiver_type.non_null().canonical_semantic(),
        });
        self.set_expression_state(receiver.value, LoweringState::Lowered(selector_read));
        let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let condition_read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: condition_read,
            rhs: null,
        });
        let selector = self.expression(selector)?;
        let selector = self.coerce_result(selector, result_type);
        let null_result = if result_type == Ty::Unit {
            self.ir.add_expr(IrExpr::UnitInstance)
        } else {
            self.ir.add_expr(IrExpr::Const(IrConst::Null))
        };
        let guarded = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), null_result), (None, selector)],
        });
        // The checker already selected the safe-call result. Record that semantic join on the
        // common-IR `when`; a backend may choose its physical representation but must not
        // reconstruct `Nothing`, nullability, or another result from the branch instructions.
        self.ir.whens.exhaustive.insert(guarded, result_type);
        self.ir.null_guards.insert(guarded);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable],
            value: Some(guarded),
        }))
    }

    fn storage_class(
        &self,
        owner: crate::fir::DeclarationId,
    ) -> Result<crate::ir::ClassId, FirLoweringFailure> {
        let class = self
            .ir
            .checked_classifier_classes
            .get(&owner)
            .or_else(|| self.ir.checked_enum_entry_classes.get(&owner))
            .copied();
        if class.is_none() {
            crate::trace_compiler!(
                "lower",
                "missing storage class body={:?} owner={owner:?}",
                self.body.owner(),
            );
        }
        class.ok_or(FirLoweringFailure::MissingLocalClass(owner))
    }

    fn class_storage_read(
        &mut self,
        owner: crate::fir::DeclarationId,
        field: u32,
    ) -> Result<ExprId, FirLoweringFailure> {
        let class = self.storage_class(owner)?;
        let receiver = self
            .dispatch_receiver_slot()
            .map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)))
            .ok_or(FirLoweringFailure::MissingLocalClass(owner))?;
        Ok(self.ir.add_expr(IrExpr::GetField {
            receiver,
            class,
            index: field,
        }))
    }

    pub(super) fn constructor_capture_parameter(
        &mut self,
        owner: crate::fir::DeclarationId,
        field: u32,
        site: crate::fir::FirConstructorCaptureSite,
    ) -> Result<ExprId, FirLoweringFailure> {
        // WHICH of the two this is, is the checker's answer and travels on the node. There is no
        // search and no fallback here on purpose: a body can hold several captures of the same
        // owner's fields, so a search keyed on anything less than the exact coordinate binds
        // whichever the map yielded first, and "try the capture, else read the parameter"
        // rediscovers a binding the checker already made.
        let source = crate::fir::FirCaptureSource::ConstructorPrefix { owner, field };
        let enclosing_depth = match site {
            crate::fir::FirConstructorCaptureSite::Captured { enclosing_depth } => enclosing_depth,
            crate::fir::FirConstructorCaptureSite::Parameter => {
                let declaration = crate::fir::DeclarationId::from_raw(self.body.owner().raw());
                let valid_owner = self
                    .index
                    .declaration_anchor(declaration)
                    .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::Constructor)
                    .and_then(|anchor| anchor.owner)
                    == Some(owner);
                if !valid_owner || field >= self.class_constructor_capture_count {
                    return Err(FirLoweringFailure::InvalidConstructorCapture { owner, field });
                }
                return Ok(self.ir.add_expr(IrExpr::GetValue(field + 1)));
            }
        };
        let capture = self
            .capture_slots
            .get(&(enclosing_depth, source))
            .cloned()
            .ok_or(FirLoweringFailure::MissingCapture {
                enclosing_depth,
                source,
            })?;
        Ok(self.ir.add_expr(IrExpr::GetValue(capture.slot)))
    }

    fn constructor_context_parameter(
        &mut self,
        owner: crate::fir::DeclarationId,
        parameter: u32,
    ) -> Result<ExprId, FirLoweringFailure> {
        let declaration = crate::fir::DeclarationId::from_raw(self.body.owner().raw());
        let valid_owner = self
            .index
            .declaration_anchor(declaration)
            .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::Constructor)
            .and_then(|anchor| anchor.owner)
            == Some(owner);
        if !valid_owner || parameter >= self.class_constructor_context_count {
            return Err(FirLoweringFailure::InvalidConstructorCapture {
                owner,
                field: parameter,
            });
        }
        let slot = 1u32
            .checked_add(self.class_constructor_capture_count)
            .and_then(|slot| slot.checked_add(parameter))
            .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
        Ok(self.ir.add_expr(IrExpr::GetValue(slot)))
    }

    pub(super) fn captured_class_storage_holder(
        &mut self,
        owner: crate::fir::DeclarationId,
        receiver: FirExprId,
        path: &[crate::fir::DeclarationId],
        field: u32,
    ) -> Result<ExprId, FirLoweringFailure> {
        let mut receiver = self.expression(receiver)?;
        for declaration in path {
            let class = self
                .ir
                .checked_classifier_classes
                .get(declaration)
                .copied()
                .ok_or(FirLoweringFailure::MissingLocalClass(*declaration))?;
            receiver = self.ir.add_expr(IrExpr::GetField {
                receiver,
                class,
                index: 0,
            });
        }
        let class = self.storage_class(owner)?;
        // Kotlin exposes an enum entry receiver as the parent enum type, while a property declared
        // in an entry body is owned by that entry's stable anonymous-subclass declaration. The FIR
        // owner proves the receiver's exact runtime subtype; make that checked narrowing explicit
        // in common IR before addressing the owner field. Backends receive no inference task.
        if self.ir.checked_enum_entry_classes.contains_key(&owner) {
            receiver = self.ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: receiver,
                type_operand: crate::types::Ty::obj_name(
                    self.ir.classes[class as usize].fq_name_id(),
                ),
            });
        }
        Ok(self.ir.add_expr(IrExpr::GetField {
            receiver,
            class,
            index: field,
        }))
    }

    pub(super) fn enclosing_class_storage_read(
        &mut self,
        owner: crate::fir::DeclarationId,
        enclosing_depth: u32,
        field: u32,
    ) -> Result<ExprId, FirLoweringFailure> {
        if enclosing_depth == 0 {
            return self.class_storage_read(owner, field);
        }
        let declaration = crate::fir::DeclarationId::from_raw(self.body.owner().raw());
        let mut classifier = self
            .index
            .enclosing_classifier(declaration)
            .map(|header| header.declaration)
            .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?;
        let mut receiver = self
            .dispatch_receiver_slot()
            .map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)))
            .ok_or(FirLoweringFailure::MissingLocalClass(classifier))?;
        for _ in 0..enclosing_depth {
            let class = self
                .ir
                .checked_classifier_classes
                .get(&classifier)
                .copied()
                .ok_or(FirLoweringFailure::MissingLocalClass(classifier))?;
            receiver = self.ir.add_expr(IrExpr::GetField {
                receiver,
                class,
                index: 0,
            });
            classifier = self
                .index
                .declaration_anchor(classifier)
                .and_then(|anchor| anchor.owner)
                .filter(|owner| self.index.classifier_header(*owner).is_some())
                .ok_or(FirLoweringFailure::MissingLocalClass(classifier))?;
        }
        if classifier != owner {
            return Err(FirLoweringFailure::MissingLocalClass(owner));
        }
        let class = self.storage_class(owner)?;
        Ok(self.ir.add_expr(IrExpr::GetField {
            receiver,
            class,
            index: field,
        }))
    }

    /// Realize the checker-published semantic enclosing-instance path in the current common class
    /// layout. The path already fixes every classifier edge; this performs no receiver lookup or
    /// type-based recovery. The eventual extraction of inner-instance storage from common IR into
    /// target backends can consume the same path without changing checked FIR.
    pub(super) fn enclosing_receiver(
        &mut self,
        path: &[crate::fir::DeclarationId],
        origin: crate::fir::OriginId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let owner = crate::fir::DeclarationId::from_raw(self.body.owner().raw());
        let constructor_owner = self
            .index
            .declaration_anchor(owner)
            .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::Constructor)
            .and_then(|anchor| anchor.owner);
        let direct_constructor_outer = constructor_owner
            .filter(|classifier| path.first() == Some(classifier))
            .and_then(|classifier| self.ir.checked_classifier_classes.get(&classifier).copied())
            .and_then(|class| {
                self.ir.classes[class as usize]
                    .pre_super_param_fields
                    .iter()
                    .find_map(|(parameter, field)| (*field == 0).then_some(*parameter))
            });
        let (mut receiver, remaining) = if let Some(parameter) = direct_constructor_outer {
            // Before a constructor has delegated, the current `this` is uninitialized and cannot
            // legally be used for `getfield`. The checked path's first edge is the same enclosing
            // instance already present in the compiler-supplied constructor prefix. Realize that
            // exact common-layout coordinate directly; later edges, if any, read initialized outers.
            let slot = self
                .capture_count
                .checked_add(1)
                .and_then(|slot| slot.checked_add(parameter))
                .ok_or(FirLoweringFailure::MissingImplicitReceiver { origin })?;
            (self.ir.add_expr(IrExpr::GetValue(slot)), &path[1..])
        } else {
            let receiver = self
                .dispatch_receiver_slot()
                .map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)))
                .ok_or(FirLoweringFailure::MissingImplicitReceiver { origin })?;
            (receiver, path)
        };
        for declaration in remaining {
            let inner = self
                .index
                .classifier_header(*declaration)
                .ok_or(FirLoweringFailure::MissingLocalClass(*declaration))?
                .classifier;
            let outer_declaration = self
                .index
                .declaration_anchor(*declaration)
                .and_then(|anchor| anchor.owner)
                .ok_or(FirLoweringFailure::MissingLocalClass(*declaration))?;
            let outer = if let Some(outer) = self.index.classifier_header(outer_declaration) {
                outer.classifier
            } else if let Some(outer) = self
                .ir
                .checked_enum_entry_classes
                .get(&outer_declaration)
                .copied()
            {
                self.ir.classes[outer as usize].fq_name_id()
            } else {
                return Err(FirLoweringFailure::MissingLocalClass(outer_declaration));
            };
            receiver = self.ir.add_expr(IrExpr::EnclosingInstance {
                receiver,
                inner,
                outer,
            });
        }
        Ok(receiver)
    }
}

/// An IR constant for a checked FIR one, carrying the value and the checked identity.
///
/// The constant's TYPE is part of what the frontend checked, so common IR records it: `FirConstant`
/// has no signed integral case narrower than `Int` and no `UByte`/`UShort` case, so the checked type
/// is the only thing carrying those widths. What primitive ends up holding an unsigned value is not
/// decided here — see [`IrConst::UByte`].
fn lower_constant(
    constant: &FirConstant,
    ty: Ty,
    origin: crate::fir::OriginId,
) -> Result<IrConst, FirLoweringFailure> {
    let ty = ty.canonical_semantic();
    Ok(match constant {
        FirConstant::Int(value) => {
            let value = i32::try_from(*value)
                .map_err(|_| FirLoweringFailure::InvalidIntegerConstant { origin })?;
            // The checked type says WHICH integral type this constant is, exactly as it does for
            // the unsigned ones below — `FirConstant` has no narrower integral case, so the type
            // is the only thing that carries the width.
            //
            // Recording a `Byte`-typed constant as an `Int` loses it, and a backend that boxes by
            // the constant's SHAPE then boxes that value as an `Int`: the JVM backend did, so an
            // `is Byte` test answered false.
            match ty.non_null() {
                Ty::Byte => IrConst::Byte(
                    i8::try_from(value)
                        .map_err(|_| FirLoweringFailure::InvalidIntegerConstant { origin })?,
                ),
                Ty::Short => IrConst::Short(
                    i16::try_from(value)
                        .map_err(|_| FirLoweringFailure::InvalidIntegerConstant { origin })?,
                ),
                _ => IrConst::Int(value),
            }
        }
        FirConstant::UInt(value) => {
            let value = u32::try_from(*value)
                .map_err(|_| FirLoweringFailure::InvalidIntegerConstant { origin })?;
            // The checked type says WHICH unsigned type this constant is, and that identity is
            // recorded rather than resolved into a width. `UByte` and `UShort` are value classes,
            // so which primitive ends up holding the value is a representation decision and
            // belongs to a backend; folding them into `Int` here took that decision away, because
            // a backend reading the constant then sees only a number.
            //
            // The VALUE is what is carried: 200u is 200. A backend that wants the byte -56 derives
            // it, and one that wants something else derives that instead.
            match ty.non_null() {
                Ty::UByte => IrConst::UByte(value as u8),
                Ty::UShort => IrConst::UShort(value as u16),
                Ty::UInt => IrConst::UInt(value),
                _ => return Err(FirLoweringFailure::InvalidIntegerConstant { origin }),
            }
        }
        FirConstant::Long(value) => IrConst::Long(*value),
        FirConstant::ULong(value) => IrConst::ULong(*value as u64),
        FirConstant::Double(value) => IrConst::Double(*value),
        FirConstant::Float(value) => IrConst::Float(*value),
        FirConstant::Boolean(value) => IrConst::Boolean(*value),
        FirConstant::String(value) => IrConst::String(value.clone()),
        FirConstant::Char(value) => IrConst::Char(*value),
        FirConstant::Null => IrConst::Null,
    })
}

fn lower_binary_operation(operation: FirBinaryOperation) -> IrBinOp {
    match operation {
        FirBinaryOperation::Add => IrBinOp::Add,
        FirBinaryOperation::Subtract => IrBinOp::Sub,
        FirBinaryOperation::Multiply => IrBinOp::Mul,
        FirBinaryOperation::Divide => IrBinOp::Div,
        FirBinaryOperation::Remainder => IrBinOp::Rem,
        FirBinaryOperation::Equal => IrBinOp::Eq,
        FirBinaryOperation::NotEqual => IrBinOp::Ne,
        FirBinaryOperation::Less => IrBinOp::Lt,
        FirBinaryOperation::LessOrEqual => IrBinOp::Le,
        FirBinaryOperation::Greater => IrBinOp::Gt,
        FirBinaryOperation::GreaterOrEqual => IrBinOp::Ge,
        FirBinaryOperation::BooleanAnd => IrBinOp::And,
        FirBinaryOperation::BooleanOr => IrBinOp::Or,
        FirBinaryOperation::ReferentialEqual => IrBinOp::RefEq,
        FirBinaryOperation::ReferentialNotEqual => IrBinOp::RefNe,
        FirBinaryOperation::BitwiseAnd => IrBinOp::BitAnd,
        FirBinaryOperation::BitwiseOr => IrBinOp::BitOr,
        FirBinaryOperation::BitwiseXor => IrBinOp::BitXor,
        FirBinaryOperation::ShiftLeft => IrBinOp::Shl,
        FirBinaryOperation::ShiftRight => IrBinOp::Shr,
        FirBinaryOperation::UnsignedShiftRight => IrBinOp::Ushr,
    }
}

fn lower_type_operation(operation: FirTypeOperation, target: crate::types::Ty) -> IrTypeOp {
    match operation {
        FirTypeOperation::Is => IrTypeOp::InstanceOf,
        FirTypeOperation::NotIs => IrTypeOp::NotInstanceOf,
        // A cast to a type parameter follows the parameter's upper-bound nullability. Kotlin's
        // implicit bound is `Any?`, so `value as E` in an unconstrained generic function must let
        // `null` reach an instantiation such as `E = String?`; only `<E : Any>`/`E & Any` requires
        // the runtime null check. The checked target already carries that distinction.
        FirTypeOperation::Cast
            if target.is_nullable()
                || (target.is_ty_param() && target.upper_bound_admits_null()) =>
        {
            IrTypeOp::Cast
        }
        FirTypeOperation::Cast => IrTypeOp::CastNonNull,
        FirTypeOperation::SafeCast => {
            unreachable!("safe casts expand to checked control flow before common IR")
        }
        FirTypeOperation::NotNullAssertion => unreachable!("lowered as NotNullAssert"),
    }
}

/// The not-null check a checked platform narrowing lowers to.
pub(super) fn platform_null_check(
    narrowing: &crate::fir::FirPlatformNarrowing,
) -> crate::ir::NullCheck {
    match &narrowing.message {
        Some(message) => crate::ir::NullCheck::Named(message.to_string()),
        None => crate::ir::NullCheck::Unnamed,
    }
}
