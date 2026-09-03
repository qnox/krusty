use crate::fir::{
    FirBinaryOperation, FirConstant, FirConversion, FirConversionKind, FirExprId, FirExprKind,
    FirIndexedAccessKind, FirJumpKind, FirTypeOperation, FirUnaryOperation,
};
use crate::ir::{Callee, ExprId, IrBinOp, IrCatch, IrConst, IrExpr, IrIntrinsic, IrTypeOp};

use super::{BodyLowering, FirLoweringFailure, LoweringState};

impl BodyLowering<'_> {
    pub(super) fn expression(
        &mut self,
        expression_id: FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
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
        let lowered = match &expression.kind {
            FirExprKind::Constant(constant) => {
                let constant = lower_constant(constant, origin)?;
                self.ir.add_expr(IrExpr::Const(constant))
            }
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
                    self.ir.add_expr(IrExpr::GetValue(self.value_slot(*value)))
                }
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
                    crate::fir::FirPropertyTarget::Module(property) => self
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
            FirExprKind::LateinitFieldRead { target } => self.ir.add_expr(IrExpr::Checked(
                crate::ir::IrCheckedOperation::LateinitFieldRead { target: *target },
            )),
            FirExprKind::BackingFieldRead { target } => self.ir.add_expr(IrExpr::Checked(
                crate::ir::IrCheckedOperation::BackingFieldRead { target: *target },
            )),
            FirExprKind::BackingFieldWrite {
                target,
                value,
                conversion,
            } => {
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::Checked(
                    crate::ir::IrCheckedOperation::BackingFieldWrite {
                        target: *target,
                        value,
                    },
                ))
            }
            FirExprKind::PluginExpression {
                plugin,
                operation,
                data,
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
                })
            }
            FirExprKind::Call(call) => {
                let lowered = self.checked_call(call)?;
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
                if let Some(declared) =
                    declared_result.filter(|declared| *declared != expression.ty.get())
                {
                    // The checked call returns through the declaration's erased JVM slot before
                    // the frontend-selected coercion produces the substituted result. Preserve
                    // that physical fact on the complete lowered call (which may be a block of
                    // argument temporaries) so representation lowering can distinguish a boxed
                    // value-class value in `fun <T> id(x: T): T` from a declaration whose value-
                    // class return is already its unboxed carrier.
                    self.ir
                        .physical_types
                        .insert(lowered, declared.erased_recv());
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: lowered,
                        type_operand: expression.ty.get(),
                    })
                } else {
                    lowered
                }
            }
            FirExprKind::ConstructorCall(call) => self.checked_constructor_call(call)?,
            FirExprKind::AnonymousObject(object) => {
                let (classifier, mut arguments) =
                    self.prepare_captured_class(object.declaration, &object.captures)?;
                let header = self
                    .index
                    .classifier_header(object.declaration)
                    .ok_or(FirLoweringFailure::MissingLocalClass(object.declaration))?;
                let mut delegate_parameters = Vec::with_capacity(object.delegate_arguments.len());
                for argument in &object.delegate_arguments {
                    let resolved = header
                        .interface_delegations
                        .get(argument.delegation as usize)
                        .ok_or(FirLoweringFailure::MissingLocalClass(object.declaration))?;
                    let expected_parameter = arguments
                        .len()
                        .checked_add(delegate_parameters.len())
                        .and_then(|parameter| u32::try_from(parameter).ok())
                        .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
                    if resolved.source
                        != crate::fir::ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(
                            expected_parameter,
                        )
                    {
                        return Err(FirLoweringFailure::MissingLocalClass(object.declaration));
                    }
                    let ty = self
                        .body
                        .expr(argument.value)
                        .ok_or(FirLoweringFailure::MissingExpression(argument.value))?
                        .ty
                        .get();
                    let value = self.expression(argument.value)?;
                    delegate_parameters.push((value, ty));
                }
                {
                    let class = self
                        .ir
                        .checked_classifier_classes
                        .get(&object.declaration)
                        .copied()
                        .ok_or(FirLoweringFailure::MissingLocalClass(object.declaration))?;
                    let class = &mut self.ir.classes[class as usize];
                    class
                        .ctor_args
                        .extend(
                            delegate_parameters
                                .iter()
                                .map(|(_, ty)| crate::ir::IrCtorArg {
                                    name: None,
                                    ty: *ty,
                                    declared_ty: None,
                                    is_field: false,
                                    has_default: false,
                                    is_vararg: false,
                                    type_param: None,
                                    check: None,
                                }),
                        );
                    class.constructor_prefix_count = class
                        .constructor_prefix_count
                        .checked_add(delegate_parameters.len() as u32)
                        .ok_or(FirLoweringFailure::ValueIdentityOverflow)?;
                }
                arguments.extend(delegate_parameters);
                let (arguments, parameter_types): (Vec<_>, Vec<_>) = arguments.into_iter().unzip();
                self.ir.add_expr(IrExpr::New {
                    internal: classifier,
                    args: arguments,
                    ctor_params: Some(parameter_types),
                    ctor_desc: None,
                    external_target: None,
                })
            }
            FirExprKind::ComparisonCall { operation, call } => {
                let call = self.checked_call(call)?;
                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: lower_binary_operation(*operation),
                    lhs: call,
                    rhs: zero,
                })
            }
            FirExprKind::ContainmentCall { call, negated } => {
                let call = self.checked_call(call)?;
                if *negated {
                    let false_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Eq,
                        lhs: call,
                        rhs: false_value,
                    })
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
            } => self.checked_callable_reference(
                target.clone(),
                *binding,
                *dispatch_receiver,
                *extension_receiver,
                substitutions,
                adaptation.as_deref(),
                function_type.get(),
                *reflective,
            )?,
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
            } => self.ir.add_expr(IrExpr::LocalPropertyReference {
                name: name.clone(),
                property_type: property_type.get(),
            }),
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
                let operand = self.expression(operand_id)?;
                match operation {
                    FirTypeOperation::NotNullAssertion => {
                        let asserted = self.ir.add_expr(IrExpr::NotNullAssert {
                            operand,
                            message: None,
                        });
                        let operand_ty = self
                            .body
                            .expr(operand_id)
                            .ok_or(FirLoweringFailure::MissingExpression(operand_id))?
                            .ty
                            .get();
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
                    FirTypeOperation::Is | FirTypeOperation::NotIs | FirTypeOperation::Cast => {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: lower_type_operation(*operation, target.get()),
                            arg: operand,
                            type_operand: target.get(),
                        })
                    }
                }
            }
            FirExprKind::ImplicitConversion { value, conversion } => {
                self.expression_with_conversion(*value, Some(*conversion))?
            }
            FirExprKind::Unary { operation, operand } => {
                let operand = self.expression(*operand)?;
                match operation {
                    FirUnaryOperation::Negate => self.ir.add_expr(IrExpr::PrimitiveNeg {
                        operand,
                        ty: expression.ty.get(),
                    }),
                    FirUnaryOperation::Identity => operand,
                    FirUnaryOperation::BooleanNot => {
                        let false_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::Eq,
                            lhs: operand,
                            rhs: false_value,
                        })
                    }
                    FirUnaryOperation::Increment | FirUnaryOperation::Decrement => {
                        let result = expression.ty.get();
                        let one = self.ir.add_expr(IrExpr::Const(match result.non_null() {
                            crate::types::Ty::Long | crate::types::Ty::ULong => IrConst::Long(1),
                            crate::types::Ty::Float => IrConst::Float(1.0),
                            crate::types::Ty::Double => IrConst::Double(1.0),
                            _ => IrConst::Int(1),
                        }));
                        let updated = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: if *operation == FirUnaryOperation::Increment {
                                IrBinOp::Add
                            } else {
                                IrBinOp::Sub
                            },
                            lhs: operand,
                            rhs: one,
                        });
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: updated,
                            type_operand: result,
                        })
                    }
                    FirUnaryOperation::BitwiseNot => {
                        let all_bits = self.ir.add_expr(IrExpr::Const(
                            if expression.ty.get().non_null() == crate::types::Ty::Long {
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
            FirExprKind::Binary {
                operation,
                lhs,
                rhs,
            } => {
                let lhs = self.expression(*lhs)?;
                match operation {
                    FirBinaryOperation::BooleanAnd => {
                        let rhs = self.expression(*rhs)?;
                        let false_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
                        self.ir.add_expr(IrExpr::When {
                            branches: vec![(Some(lhs), rhs), (None, false_value)],
                        })
                    }
                    FirBinaryOperation::BooleanOr => {
                        let true_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
                        let rhs = self.expression(*rhs)?;
                        self.ir.add_expr(IrExpr::When {
                            branches: vec![(Some(lhs), true_value), (None, rhs)],
                        })
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
            FirExprKind::StringTemplate(parts) => {
                let parts = parts
                    .iter()
                    .copied()
                    .map(|part| self.expression(part))
                    .collect::<Result<Vec<_>, _>>()?;
                self.ir.add_expr(IrExpr::StringConcat(parts))
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
            } => self.checked_function_invoke_reference(
                *callee,
                target_parameters,
                *target_result,
                *target_suspend,
                reference_parameters,
                *reference_result,
                *suspend,
            )?,
            FirExprKind::IndexedRead {
                kind,
                receiver,
                indices,
            } => {
                let receiver = self.expression(*receiver)?;
                let arguments = indices
                    .iter()
                    .copied()
                    .map(|index| self.expression(index))
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
                    .copied()
                    .map(|index| self.expression(index))
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
                // The selector executes only on the non-null branch. Publish that data-flow fact as
                // an explicit conversion so nullable primitive receivers are unboxed before an
                // extension call/lambda binding; a raw nullable-slot read would put `Integer` into
                // an `Int` local and fail JVM verification.
                let selector_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                let selector_read = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: selector_read,
                    type_operand: receiver_type.non_null(),
                });
                self.set_expression_state(receiver.value, LoweringState::Lowered(selector_read));
                let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let condition_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: condition_read,
                    rhs: null,
                });
                let selector = self.expression(*selector)?;
                let selector = self.coerce_result(selector, expression.ty.get());
                let null_result = if expression.ty.get() == crate::types::Ty::Unit {
                    self.ir.add_expr(IrExpr::UnitInstance)
                } else {
                    self.ir.add_expr(IrExpr::Const(IrConst::Null))
                };
                let guarded = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(condition), null_result), (None, selector)],
                });
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![variable],
                    value: Some(guarded),
                })
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
                let lhs = self.ir.add_expr(IrExpr::GetValue(temporary));
                let lhs = self.coerce_result(lhs, expression.ty.get());
                let result = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(condition), rhs), (None, lhs)],
                });
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
            } => {
                let condition = self.expression(*condition)?;
                let then_branch =
                    self.expression_with_conversion(*then_branch, *then_conversion)?;
                let else_branch =
                    self.expression_with_conversion(*else_branch, *else_conversion)?;
                self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(condition), then_branch), (None, else_branch)],
                })
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
                        let exc_internal = catch.parameter_ty.get().obj_internal().ok_or(
                            FirLoweringFailure::InvalidCatchType {
                                origin: catch.origin,
                            },
                        )?;
                        Ok(IrCatch {
                            var: self.value_slot(catch.parameter),
                            name: None,
                            exc_internal,
                            body: self.expression(catch.body)?,
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
            FirExprKind::When { subject, branches } => {
                let mut prefix = Vec::new();
                let subject = subject
                    .map(|subject| {
                        let value = self.expression(subject)?;
                        let temporary = self.allocate_temporary();
                        let ty = self
                            .body
                            .expr(subject)
                            .ok_or(FirLoweringFailure::MissingExpression(subject))?
                            .ty
                            .get();
                        prefix.push(self.ir.add_expr(IrExpr::Variable {
                            index: temporary,
                            ty,
                            init: Some(value),
                            named: false,
                        }));
                        Ok::<_, FirLoweringFailure>(temporary)
                    })
                    .transpose()?;
                let mut lowered_branches = Vec::with_capacity(branches.len());
                for branch in branches {
                    let mut condition = None;
                    for candidate in branch.conditions.iter().copied() {
                        let candidate = match candidate {
                            crate::fir::FirWhenCondition::SubjectEquals(candidate) => {
                                let candidate = self.expression(candidate)?;
                                let subject =
                                    subject.ok_or(FirLoweringFailure::MissingWhenSubject {
                                        origin: branch.origin,
                                    })?;
                                let subject = self.ir.add_expr(IrExpr::GetValue(subject));
                                self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op: IrBinOp::Eq,
                                    lhs: subject,
                                    rhs: candidate,
                                })
                            }
                            crate::fir::FirWhenCondition::Predicate(candidate) => {
                                self.expression(candidate)?
                            }
                        };
                        condition = Some(match condition {
                            Some(previous) => self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: IrBinOp::Or,
                                lhs: previous,
                                rhs: candidate,
                            }),
                            None => candidate,
                        });
                    }
                    if let Some(guard) = branch.guard {
                        let guard = self.expression(guard)?;
                        condition = Some(match condition {
                            Some(previous) => self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: IrBinOp::And,
                                lhs: previous,
                                rhs: guard,
                            }),
                            None => guard,
                        });
                    }
                    lowered_branches.push((condition, self.expression(branch.result)?));
                }
                let when = self.ir.add_expr(IrExpr::When {
                    branches: lowered_branches,
                });
                let has_else = branches.iter().any(|branch| branch.conditions.is_empty());
                // Carry the checker's result into common IR for every exhaustive `when`. An `else`
                // is exhaustive even when the result is `Unit`; omitting that case made the JVM
                // backend re-derive a physical type from the last branch (`Unit.INSTANCE`) and
                // disagree with a sibling branch implemented by a void write. A no-else `Unit`
                // `when` remains the one genuinely non-exhaustive statement form.
                if has_else || expression.ty.get() != crate::types::Ty::Unit {
                    self.ir.exhaustive_whens.insert(when, expression.ty.get());
                }
                if prefix.is_empty() {
                    when
                } else {
                    self.ir.add_expr(IrExpr::Block {
                        stmts: prefix,
                        value: Some(when),
                    })
                }
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
                let value = result.map(|result| self.expression(result)).transpose()?;
                self.ir.add_expr(IrExpr::Block {
                    stmts: lowered_statements,
                    value,
                })
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
            } => {
                let holder = self.constructor_capture_parameter(*owner, *field)?;
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
                value,
                conversion,
            } => {
                let holder = self.enclosing_class_storage_read(*owner, *enclosing_depth, *field)?;
                let element = self
                    .body
                    .expr(*value)
                    .expect("a checked shared-cell write value must exist")
                    .ty
                    .get();
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::RefSet {
                    elem: element,
                    holder,
                    value,
                })
            }
            FirExprKind::ConstructorCaptureSharedWrite {
                owner,
                field,
                value,
                conversion,
            } => {
                let holder = self.constructor_capture_parameter(*owner, *field)?;
                let element = self
                    .body
                    .expr(*value)
                    .expect("a checked constructor-capture write value must exist")
                    .ty
                    .get();
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::RefSet {
                    elem: element,
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
                value,
                conversion,
            } => {
                let holder = self.captured_class_storage_holder(*owner, *receiver, path, *field)?;
                let element = self
                    .body
                    .expr(*value)
                    .expect("a checked shared-cell write value must exist")
                    .ty
                    .get();
                let value = self.expression_with_conversion(*value, *conversion)?;
                self.ir.add_expr(IrExpr::RefSet {
                    elem: element,
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
                    .get(&(*enclosing_depth, *source))
                    .copied()
                    .ok_or(FirLoweringFailure::MissingCapture {
                        enclosing_depth: *enclosing_depth,
                        source: *source,
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
            } => self.checked_local_call(target.clone(), *extension_receiver, arguments)?,
            FirExprKind::Lambda { callable, body } => {
                let suspend = matches!(
                    expression.ty.get().non_null(),
                    crate::types::Ty::Fun(signature) if signature.suspend
                );
                self.checked_lambda(*callable, body, suspend)?
            }
        };
        self.ir.logical_types.insert(lowered, expression.ty.get());
        self.record_expression_origins(first_generated, lowered, origin);
        self.set_expression_state(expression_id, LoweringState::Lowered(lowered));
        Ok(lowered)
    }

    pub(super) fn expression_with_conversion(
        &mut self,
        expression: FirExprId,
        conversion: Option<FirConversion>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let source_type = self
            .body
            .expr(expression)
            .ok_or(FirLoweringFailure::MissingExpression(expression))?
            .ty
            .get();
        let expression = self.expression(expression)?;
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
            FirConversionKind::Sam(sam) => {
                let conversion = self
                    .body
                    .sam_conversion(sam)
                    .ok_or(FirLoweringFailure::UnsupportedConversion {
                        origin: conversion.origin,
                    })?
                    .clone();
                if let IrExpr::Lambda { impl_fn, sam, .. } = &mut self.ir.exprs[expression as usize]
                {
                    let target = crate::ir::IrSamTarget {
                        classifier: conversion.classifier,
                        method: conversion.method.into(),
                        parameters: conversion.parameters.iter().map(|ty| ty.get()).collect(),
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
                        function_adapter: false,
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
            FirConversionKind::SuspendFunction { from, to } => self
                .suspend_function_value_adapter(from, to, expression)
                .ok_or(FirLoweringFailure::UnsupportedConversion {
                    origin: conversion_origin,
                })?,
            FirConversionKind::CoerceToUnit => self.unit_value_after_effect(expression),
        })
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

    fn constructor_capture_parameter(
        &mut self,
        owner: crate::fir::DeclarationId,
        field: u32,
    ) -> Result<ExprId, FirLoweringFailure> {
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
        Ok(self.ir.add_expr(IrExpr::GetValue(field + 1)))
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

fn lower_constant(
    constant: &FirConstant,
    origin: crate::fir::OriginId,
) -> Result<IrConst, FirLoweringFailure> {
    Ok(match constant {
        FirConstant::Int(value) => IrConst::Int(
            i32::try_from(*value)
                .map_err(|_| FirLoweringFailure::InvalidIntegerConstant { origin })?,
        ),
        FirConstant::UInt(value) => IrConst::Int(
            u32::try_from(*value)
                .map_err(|_| FirLoweringFailure::InvalidIntegerConstant { origin })?
                as i32,
        ),
        FirConstant::Long(value) | FirConstant::ULong(value) => IrConst::Long(*value),
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
