//! Translation of checker-selected calls into stable checked FIR call nodes.

use super::*;

pub(super) struct MemberExtensionFirTarget {
    pub(super) target: FirCallTarget,
    pub(super) substitutions: Box<[FirTypeSubstitution]>,
    /// Source-visible semantic parameters. The external target separately inserts its extension
    /// receiver into the provider parameter list.
    pub(super) parameters: Vec<Ty>,
    pub(super) extension_parameter: Option<u32>,
}
use crate::fir::FirInlineBodyPlan;

/// One already-selected operator target. Context parameters stay separate while source operands
/// are mapped, then join the value parameters in the checked call's semantic parameter list.
pub(super) struct SelectedOperatorTarget {
    pub(super) target: FirCallTarget,
    pub(super) extension: bool,
    pub(super) context_parameters: Box<[ResolvedTy]>,
    pub(super) value_parameters: Box<[ResolvedTy]>,
    pub(super) vararg_index: Option<usize>,
    pub(super) context_arguments: Vec<Option<crate::resolve::ResolvedContextArgument>>,
}

impl SelectedOperatorTarget {
    pub(super) fn parameter_types(&self) -> Box<[ResolvedTy]> {
        self.context_parameters
            .iter()
            .chain(self.value_parameters.iter())
            .copied()
            .collect()
    }
}

pub(super) fn fir_inline_body_plan(
    plan: Option<&crate::libraries::InlineBodyPlan>,
) -> Option<Box<crate::fir::FirInlineBodyPlan>> {
    let crate::libraries::InlineBodyPlan::InvokeLambda {
        lambda_parameter,
        argument_parameters,
        return_parameter,
    } = plan?
    else {
        // The suspend/finally plan also needs stable identities for its two selected member calls.
        // Keep it out of FIR until the provider interns those declarations; retaining LibraryMember
        // here would leak JVM linkage into the checked representation.
        return None;
    };
    Some(Box::new(crate::fir::FirInlineBodyPlan::InvokeLambda {
        lambda_parameter: u32::try_from(*lambda_parameter)
            .expect("inline plan parameter ordinal exceeds packed FIR range"),
        argument_parameters: argument_parameters
            .iter()
            .map(|parameter| {
                u32::try_from(*parameter)
                    .expect("inline plan parameter ordinal exceeds packed FIR range")
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        return_parameter: return_parameter.map(|parameter| {
            u32::try_from(parameter)
                .expect("inline plan parameter ordinal exceeds packed FIR range")
        }),
    }))
}
use crate::resolve::ResolvedCall;

fn intrinsic_binary_operation(
    intrinsic: crate::libraries::CompilerIntrinsic,
) -> Option<FirBinaryOperation> {
    Some(match intrinsic {
        crate::libraries::CompilerIntrinsic::PrimitiveBinary(operation) => match operation {
            crate::libraries::PrimitiveBinaryIntrinsic::Add => FirBinaryOperation::Add,
            crate::libraries::PrimitiveBinaryIntrinsic::Subtract => FirBinaryOperation::Subtract,
            crate::libraries::PrimitiveBinaryIntrinsic::Multiply => FirBinaryOperation::Multiply,
            crate::libraries::PrimitiveBinaryIntrinsic::Divide => FirBinaryOperation::Divide,
            crate::libraries::PrimitiveBinaryIntrinsic::Remainder => FirBinaryOperation::Remainder,
        },
        crate::libraries::CompilerIntrinsic::PrimitiveBitAnd => FirBinaryOperation::BitwiseAnd,
        crate::libraries::CompilerIntrinsic::PrimitiveBitOr => FirBinaryOperation::BitwiseOr,
        crate::libraries::CompilerIntrinsic::PrimitiveBitXor => FirBinaryOperation::BitwiseXor,
        crate::libraries::CompilerIntrinsic::PrimitiveShiftLeft => FirBinaryOperation::ShiftLeft,
        crate::libraries::CompilerIntrinsic::PrimitiveShiftRight => FirBinaryOperation::ShiftRight,
        crate::libraries::CompilerIntrinsic::PrimitiveUnsignedShiftRight => {
            FirBinaryOperation::UnsignedShiftRight
        }
        crate::libraries::CompilerIntrinsic::ArraySize
        | crate::libraries::CompilerIntrinsic::CharCode
        | crate::libraries::CompilerIntrinsic::StringLength
        | crate::libraries::CompilerIntrinsic::StringPlus
        | crate::libraries::CompilerIntrinsic::NullableAnyToString
        | crate::libraries::CompilerIntrinsic::NumericConversion
        | crate::libraries::CompilerIntrinsic::PrimitiveCompare
        | crate::libraries::CompilerIntrinsic::BooleanNot
        | crate::libraries::CompilerIntrinsic::PrimitiveBitNot
        | crate::libraries::CompilerIntrinsic::Assert
        | crate::libraries::CompilerIntrinsic::AssertFailsWith
        | crate::libraries::CompilerIntrinsic::Print
        | crate::libraries::CompilerIntrinsic::Println
        | crate::libraries::CompilerIntrinsic::StartCoroutine
        | crate::libraries::CompilerIntrinsic::CoroutineContext
        | crate::libraries::CompilerIntrinsic::CoroutineSuspended
        | crate::libraries::CompilerIntrinsic::SuspendCoroutine
        | crate::libraries::CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn
        | crate::libraries::CompilerIntrinsic::EnumValues
        | crate::libraries::CompilerIntrinsic::EnumValueOf
        | crate::libraries::CompilerIntrinsic::ForEach
        | crate::libraries::CompilerIntrinsic::ForEachIndexed
        | crate::libraries::CompilerIntrinsic::Map
        | crate::libraries::CompilerIntrinsic::FlatMap
        | crate::libraries::CompilerIntrinsic::IsEmpty
        | crate::libraries::CompilerIntrinsic::IsNotEmpty
        | crate::libraries::CompilerIntrinsic::Count
        | crate::libraries::CompilerIntrinsic::TrimIndent
        | crate::libraries::CompilerIntrinsic::TrimMargin => return None,
    })
}

impl BodyFirChecker<'_> {
    fn primitive_compare_operand(selected: &crate::symbol_resolver::ResolvedMember) -> Option<Ty> {
        if selected.member.realization
            != crate::libraries::MemberRealization::Intrinsic(
                crate::libraries::CompilerIntrinsic::PrimitiveCompare,
            )
        {
            return None;
        }
        let [parameter] = selected.member.params.as_slice() else {
            return None;
        };
        let receiver_ty = selected.receiver.canonical_semantic().non_null();
        let parameter_ty = parameter.canonical_semantic().non_null();
        if (receiver_ty == Ty::Boolean && parameter_ty == Ty::Boolean)
            || (receiver_ty == Ty::Char && parameter_ty == Ty::Char)
        {
            Some(Ty::Int)
        } else {
            Ty::promote(receiver_ty, parameter_ty)
        }
    }

    fn primitive_compare_call(
        &mut self,
        expression: ExprId,
        receiver: FirReceiver,
        arguments: &[ExprId],
        selected: &crate::symbol_resolver::ResolvedMember,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let [argument] = arguments else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        let [_parameter] = selected.member.params.as_slice() else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        let operand = Self::primitive_compare_operand(selected).ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        let operand = self.resolved_type(
            self.file
                .expr_span(expression)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            operand,
        )?;
        let result = self.resolved_type(
            self.file
                .expr_span(expression)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            Ty::Int,
        )?;
        Ok(FirExprKind::Call(FirCall {
            target: FirCallTarget::Intrinsic {
                operation: FirIntrinsic::PrimitiveCompare { operand },
                receiver: Some(operand),
                parameters: vec![operand].into_boxed_slice(),
                result,
            },
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            parameter_types: vec![operand].into_boxed_slice(),
            arguments: self.call_arguments_with_context(
                expression,
                &[*argument],
                &[operand.get()],
                std::iter::empty(),
                None,
            )?,
            substitutions: Box::new([]),
        }))
    }

    fn explicit_receiver(&mut self, expression: ExprId) -> Result<FirReceiver, BodyCheckFailure> {
        let value = self.expression(expression)?;
        let conversion = self
            .info
            .selected_value_smartcasts
            .get(&expression)
            .copied()
            .map(|target| {
                let to = ResolvedTy::new(target).map_err(|error| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnpublishableType(error),
                    )
                })?;
                Ok(FirConversion {
                    origin: self.expression_origin(expression)?,
                    kind: FirConversionKind::SmartCast { to },
                })
            })
            .transpose()?;
        Ok(FirReceiver { value, conversion })
    }

    /// Whether the provider selected the language's compiler-supplied range construction rather
    /// than an ordinary user/library operator implementation. This consumes the semantic
    /// realization attached to the selected declaration; spelling alone never identifies a
    /// builtin range.
    pub(super) fn selected_range_construction(&self, expression: ExprId, convention: &str) -> bool {
        let realization = match self.info.resolved_operator_call(expression, convention) {
            Some(ResolvedCall::Member(selected)) => selected.member.realization,
            Some(ResolvedCall::Extension(selected)) => selected.callable.member_realization,
            Some(
                ResolvedCall::TopLevel(_)
                | ResolvedCall::Companion(_)
                | ResolvedCall::MemberExtension { .. }
                | ResolvedCall::LocalFunction(_),
            )
            | None => return false,
        };
        matches!(
            realization,
            crate::libraries::MemberRealization::RangeConstruction { .. }
        )
    }

    /// Consume the implementation marker on an exact checker-selected primitive operator. These
    /// declarations have semantic callable identities for overload selection but no runtime method;
    /// checked FIR must publish the operation before the external realization table is discarded.
    pub(super) fn selected_primitive_binary_operation(
        &self,
        expression: ExprId,
        convention: &str,
    ) -> Option<FirBinaryOperation> {
        let ResolvedCall::Member(selected) =
            self.info.resolved_operator_call(expression, convention)?
        else {
            return None;
        };
        let crate::libraries::MemberRealization::Intrinsic(intrinsic) = selected.member.realization
        else {
            return None;
        };
        intrinsic_binary_operation(intrinsic)
    }

    /// A primitive floating relational operator has IEEE-754 ordering semantics, distinct from an
    /// explicit `Float.compareTo`/`Double.compareTo` call's total ordering (`-0.0 < 0.0` is false,
    /// while `(-0.0).compareTo(0.0) < 0` is true). The selected intrinsic proves both the callable
    /// identity and common operand; syntax or backend representation does not participate.
    pub(super) fn selected_ieee_relational_operation(&self, expression: ExprId) -> bool {
        let Some(ResolvedCall::Member(selected)) =
            self.info.resolved_operator_call(expression, "compareTo")
        else {
            return false;
        };
        matches!(
            Self::primitive_compare_operand(selected),
            Some(Ty::Float | Ty::Double)
        )
    }

    pub(super) fn qualified_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if let Some(ExprLowering::BuiltinUnaryCall { operation }) =
            self.info.expr_lowers.get(&expression)
        {
            if !arguments.is_empty() {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
            return Ok(FirExprKind::Unary {
                operation: match operation {
                    crate::resolve::BuiltinUnaryOperation::Negate => FirUnaryOperation::Negate,
                    crate::resolve::BuiltinUnaryOperation::Identity => FirUnaryOperation::Identity,
                },
                operand: self.expression(receiver)?,
            });
        }
        if let Some(target) = self.info.resolved_super_call(expression).cloned() {
            return self.selected_super_call(expression, arguments, &target);
        }
        let selected = self.info.resolved_calls.get(&expression).cloned();
        crate::trace_compiler!(
            "fir",
            "qualified call expression={expression:?} span={:?} selected={selected:?}",
            self.file.expr_span(expression),
        );
        match selected {
            Some(ResolvedCall::Member(_)) => self.member_call(expression, receiver, arguments),
            Some(ResolvedCall::TopLevel(selected)) => {
                self.selected_top_level_call(expression, arguments, *selected)
            }
            Some(ResolvedCall::Extension(extension)) => {
                let source_receiver = receiver;
                let receiver = self.explicit_receiver(receiver)?;
                self.selected_extension_call(
                    expression,
                    arguments,
                    *extension,
                    receiver,
                    Some(source_receiver),
                )
            }
            Some(ResolvedCall::MemberExtension {
                dispatch_receiver,
                context_args,
                vararg_index,
                ..
            }) => {
                let selected = self
                    .info
                    .resolved_calls
                    .get(&expression)
                    .expect("the cloned member-extension call came from this entry");
                let target = self.member_extension_call_target(expression, selected)?;
                let cause = self.expression_origin(expression)?;
                let dispatch_receiver = self
                    .materialize_implicit_receiver(
                        cause,
                        self.file.expr_span(expression),
                        &dispatch_receiver,
                    )?
                    .ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?;
                let arguments = self.call_arguments_with_context(
                    expression,
                    arguments,
                    &target.parameters,
                    context_args.iter().map(Option::as_ref),
                    vararg_index,
                )?;
                let parameter_types = self.published_parameter_types(
                    self.file.expr_span(expression),
                    &target.parameters,
                )?;
                Ok(FirExprKind::Call(FirCall {
                    target: target.target,
                    dispatch_receiver: Some(dispatch_receiver),
                    extension_receiver: Some(self.explicit_receiver(receiver)?),
                    parameter_types,
                    arguments: self.member_extension_arguments(
                        expression,
                        arguments,
                        target.extension_parameter,
                    )?,
                    substitutions: target.substitutions,
                }))
            }
            Some(ResolvedCall::LocalFunction(local)) => {
                self.local_function_call(expression, arguments, *local)
            }
            Some(ResolvedCall::Companion(member)) => {
                self.selected_companion_call(expression, arguments, &member)
            }
            None => Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )),
        }
    }

    pub(super) fn unqualified_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if let Some(ResolvedCall::LocalFunction(local)) =
            self.info.resolved_calls.get(&expression).cloned()
        {
            return self.local_function_call(expression, arguments, *local);
        }
        if let Some(ResolvedCall::MemberExtension {
            dispatch_receiver,
            context_args,
            vararg_index,
            ..
        }) = self.info.resolved_calls.get(&expression).cloned()
        {
            let selected = self
                .info
                .resolved_calls
                .get(&expression)
                .expect("the cloned member-extension call came from this entry");
            let target = self.member_extension_call_target(expression, selected)?;
            let cause = self.expression_origin(expression)?;
            let dispatch_receiver = self
                .materialize_implicit_receiver(
                    cause,
                    self.file.expr_span(expression),
                    &dispatch_receiver,
                )?
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
            let extension_receiver = self.implicit_receiver(expression)?.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            let arguments = self.call_arguments_with_context(
                expression,
                arguments,
                &target.parameters,
                context_args.iter().map(Option::as_ref),
                vararg_index,
            )?;
            let parameter_types = self
                .published_parameter_types(self.file.expr_span(expression), &target.parameters)?;
            return Ok(FirExprKind::Call(FirCall {
                target: target.target,
                dispatch_receiver: Some(dispatch_receiver),
                extension_receiver: Some(extension_receiver),
                parameter_types,
                arguments: self.member_extension_arguments(
                    expression,
                    arguments,
                    target.extension_parameter,
                )?,
                substitutions: target.substitutions,
            }));
        }
        if let Some(ResolvedCall::Extension(extension)) =
            self.info.resolved_calls.get(&expression).cloned()
        {
            let receiver = self.implicit_receiver(expression)?.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            return self.selected_extension_call(expression, arguments, *extension, receiver, None);
        }
        if self.info.resolved_top_level_call(expression).is_some() {
            return self.top_level_call(expression, arguments);
        }
        if let Some(ResolvedCall::Companion(member)) =
            self.info.resolved_calls.get(&expression).cloned()
        {
            return self.selected_companion_call(expression, arguments, &member);
        }
        let selected = self.info.resolved_member(expression).cloned();
        let Some(selected) = selected else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        let receiver = self.implicit_receiver(expression)?.ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        self.selected_member_call_with_semantics(expression, arguments, &selected, receiver)
    }

    fn classifier_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        member: &crate::libraries::LibraryMember,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let classifier = member
            .owner
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        let operation = match member.implicit_classifier_callable {
            Some(crate::libraries::ImplicitClassifierCallable::EnumValues) => {
                crate::fir::FirClassifierCallable::EnumValues
            }
            Some(crate::libraries::ImplicitClassifierCallable::EnumValueOf) => {
                crate::fir::FirClassifierCallable::EnumValueOf
            }
            None => {
                return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget));
            }
        };
        let parameter_types = member.params.clone();
        let parameters = parameter_types
            .iter()
            .copied()
            .map(ResolvedTy::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let result = ResolvedTy::new(member.ret)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        Ok(FirExprKind::Call(FirCall {
            target: FirCallTarget::Classifier {
                classifier,
                operation,
                parameters: parameters.clone().into_boxed_slice(),
                result,
            },
            dispatch_receiver: None,
            extension_receiver: None,
            parameter_types: parameters.clone().into_boxed_slice(),
            arguments: self.call_arguments_with_context(
                expression,
                arguments,
                &parameter_types,
                std::iter::empty(),
                member.call_sig.vararg_index,
            )?,
            substitutions: Box::new([]),
        }))
    }

    fn selected_companion_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        member: &crate::libraries::LibraryMember,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if member.implicit_classifier_callable.is_some() {
            return self.classifier_call(expression, arguments, member);
        }
        let span = self.file.expr_span(expression);
        let dispatch_receiver = member
            .singleton_dispatch
            .as_deref()
            .map(|singleton| self.singleton_call_receiver(expression, singleton))
            .transpose()?;
        let parameters =
            self.selected_call_parameters(expression, member.stable_declaration, &member.params)?;
        let (target, substitutions) = if let Some(declaration) = member.stable_declaration {
            let callable = self
                .index
                .callable_for_declaration(declaration)
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
            (
                FirCallTarget::Module(callable.id),
                self.call_substitutions(expression, declaration)?,
            )
        } else {
            self.external_call_target_with_identity(
                expression,
                member.external_identity.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                })?,
                member
                    .singleton_dispatch
                    .as_deref()
                    .map(|singleton| singleton.ty()),
                None,
                parameters.iter().copied(),
                member.ret,
                member.declared_ret,
                member.suspend(),
                member.inline.can_inline(),
                member.inline_body_plan.as_deref(),
            )?
        };
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver,
            extension_receiver: None,
            parameter_types: self.published_parameter_types(span, &parameters)?,
            arguments: self.call_arguments_with_context(
                expression,
                arguments,
                &parameters,
                std::iter::empty(),
                member.call_sig.vararg_index,
            )?,
            substitutions,
        }))
    }

    fn selected_extension_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        extension: crate::resolve::ResolvedExtensionCall,
        extension_receiver: FirReceiver,
        source_receiver: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let (mut target, substitutions) = self.extension_call_target(expression, &extension)?;
        let dispatch_receiver = extension
            .callable
            .singleton_dispatch
            .as_deref()
            .map(|singleton| self.singleton_call_receiver(expression, singleton))
            .transpose()?;
        let context_count = extension
            .callable
            .context_count
            .min(extension.callable.params.len());
        let parameters = extension.callable.params[..context_count]
            .iter()
            .copied()
            .chain(extension.params.iter().copied())
            .collect::<Vec<_>>();
        let parameters =
            self.selected_call_parameters(expression, extension.stable_declaration, &parameters)?;
        if extension.callable.compiler_intrinsic
            == Some(crate::libraries::CompilerIntrinsic::ForEach)
        {
            let source_receiver = source_receiver.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            // The resolver records the declaration-scoped iterator protocol only when this call can
            // actually splice a lambda body. A callable-reference argument is an ordinary function
            // value: keep the already-selected external `forEach` call and do not invent iterator
            // convention decisions during FIR construction.
            if let Some(protocol) = self.info.iterator_protocol(source_receiver) {
                let span = self.file.expr_span(expression);
                let origin = self.expression_origin(expression)?;
                let plan = FirInlineBodyPlan::ForEach {
                    lambda_parameter: u32::try_from(context_count).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                    iterator_ty: ResolvedTy::new(protocol.iter_ty).map_err(|error| {
                        self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                    })?,
                    iterator: self.iterator_protocol_call(span, origin, &protocol.iterator)?,
                    has_next: self.iterator_protocol_call(span, origin, &protocol.has_next)?,
                    next: self.iterator_protocol_call(span, origin, &protocol.next)?,
                };
                let FirCallTarget::External { inline_plan, .. } = &mut target else {
                    return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
                };
                *inline_plan = Some(Box::new(plan));
            }
        }
        let extension_parameter = match &target {
            FirCallTarget::External {
                extension_receiver_parameter,
                ..
            } => *extension_receiver_parameter,
            FirCallTarget::Module(_)
            | FirCallTarget::Intrinsic { .. }
            | FirCallTarget::Classifier { .. }
            | FirCallTarget::Super { .. } => None,
        };
        let arguments = self.call_arguments_with_context(
            expression,
            arguments,
            &parameters,
            extension.context_args.iter().map(Some),
            extension.vararg_index,
        )?;
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver,
            extension_receiver: Some(extension_receiver),
            parameter_types: self
                .published_parameter_types(self.file.expr_span(expression), &parameters)?,
            arguments: self.member_extension_arguments(
                expression,
                arguments,
                extension_parameter,
            )?,
            substitutions,
        }))
    }

    pub(super) fn extension_call_target(
        &self,
        expression: ExprId,
        extension: &crate::resolve::ResolvedExtensionCall,
    ) -> Result<(FirCallTarget, Box<[FirTypeSubstitution]>), BodyCheckFailure> {
        if let Some(declaration) = extension.stable_declaration {
            let callable = self
                .index
                .callable_for_declaration(declaration)
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            Ok((
                FirCallTarget::Module(callable.id),
                self.call_substitutions(expression, declaration)?,
            ))
        } else {
            let context_count = extension
                .callable
                .context_count
                .min(extension.callable.params.len());
            let parameters = extension.callable.params[..context_count]
                .iter()
                .copied()
                .chain(extension.params.iter().copied())
                .collect::<Vec<_>>();
            if let Some(singleton) = extension.callable.singleton_dispatch.as_deref() {
                let mut target_parameters = parameters;
                target_parameters.insert(context_count, extension.receiver);
                let declaration = extension.callable.external_identity.ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
                let (mut target, substitutions) = self.external_call_target_with_identity(
                    expression,
                    declaration,
                    Some(singleton.ty()),
                    None,
                    target_parameters,
                    extension.callable.ret,
                    extension.callable.declared_ret,
                    extension.callable.suspend,
                    extension.callable.inline.can_inline(),
                    extension.callable.inline_body_plan.as_deref(),
                )?;
                let FirCallTarget::External {
                    extension_receiver_parameter,
                    ..
                } = &mut target
                else {
                    unreachable!("external target builder must produce an external call")
                };
                *extension_receiver_parameter =
                    Some(u32::try_from(context_count).map_err(|_| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?);
                Ok((target, substitutions))
            } else {
                self.external_call_target(
                    expression,
                    &extension.callable,
                    Some(extension.receiver),
                    parameters,
                )
            }
        }
    }

    pub(super) fn singleton_call_receiver(
        &mut self,
        expression: ExprId,
        singleton: &crate::libraries::SingletonDispatch,
    ) -> Result<FirReceiver, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let cause = self.expression_origin(expression)?;
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::ImplicitReceiver);
        let classifier = singleton.classifier;
        let ty = ResolvedTy::new(singleton.ty())
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let value = self.body.add_expr(FirExpr {
            origin,
            ty,
            kind: FirExprKind::SingletonValue { classifier },
        });
        Ok(FirReceiver {
            value,
            conversion: None,
        })
    }

    fn expression_member_operator_target(
        &self,
        expression: ExprId,
        convention: &str,
    ) -> Result<
        (
            FirCallTarget,
            Box<[FirTypeSubstitution]>,
            bool,
            Vec<Option<crate::resolve::ResolvedContextArgument>>,
            Option<usize>,
            Vec<Ty>,
        ),
        BodyCheckFailure,
    > {
        let selected = if convention == "get" {
            self.info.resolved_index_get_call(expression)
        } else {
            self.info.resolved_operator_call(expression, convention)
        };
        match selected {
            Some(ResolvedCall::Member(selected)) => {
                let (target, substitutions) = self.member_call_target(expression, selected)?;
                let parameters = self.selected_call_parameters(
                    expression,
                    selected.member.stable_declaration,
                    &selected.member.params,
                )?;
                Ok((
                    target,
                    substitutions,
                    false,
                    selected.context_args.clone(),
                    selected.member.call_sig.vararg_index,
                    parameters,
                ))
            }
            Some(ResolvedCall::Extension(selected)) => {
                let (target, substitutions) = self.extension_call_target(expression, selected)?;
                let context_count = selected
                    .callable
                    .context_count
                    .min(selected.callable.params.len());
                let parameters = selected.callable.params[..context_count]
                    .iter()
                    .copied()
                    .chain(selected.params.iter().copied())
                    .collect::<Vec<_>>();
                let parameters = self.selected_call_parameters(
                    expression,
                    selected.stable_declaration,
                    &parameters,
                )?;
                Ok((
                    target,
                    substitutions,
                    true,
                    selected.context_args.iter().cloned().map(Some).collect(),
                    selected.vararg_index,
                    parameters,
                ))
            }
            Some(
                ResolvedCall::TopLevel(_)
                | ResolvedCall::Companion(_)
                | ResolvedCall::MemberExtension { .. }
                | ResolvedCall::LocalFunction(_),
            )
            | None => {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
        }
    }

    fn statement_operator_target(
        &self,
        statement: StmtId,
        convention: &str,
    ) -> Result<SelectedOperatorTarget, BodyCheckFailure> {
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        self.selected_call_target(
            span,
            self.info.resolved_stmt_operator_call(statement, convention),
        )
    }

    /// Map one already-selected operator call onto a checked FIR target.
    ///
    /// Module and dependency providers differ only in linkage: a same-module target carries its
    /// stable `CallableId`, a dependency target its `ExternalCallableId` plus the semantic signature
    /// the emitter needs. Every operator convention — index access, in-place compound assignment,
    /// increment — routes through here, so a convention cannot silently support source receivers
    /// only.
    pub(super) fn selected_call_target(
        &self,
        span: Option<crate::diag::Span>,
        call: Option<&ResolvedCall>,
    ) -> Result<SelectedOperatorTarget, BodyCheckFailure> {
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        match call {
            Some(ResolvedCall::Member(selected)) => {
                let target = if let Some(declaration) = selected.member.stable_declaration {
                    self.index
                        .callable_for_declaration(declaration)
                        .map(|callable| FirCallTarget::Module(callable.id))
                        .ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?
                } else {
                    FirCallTarget::External {
                        declaration: selected.member.external_identity.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?,
                        receiver: Some(resolved(selected.receiver)?),
                        declared_receiver: None,
                        parameters: selected
                            .member
                            .params
                            .iter()
                            .copied()
                            .map(resolved)
                            .collect::<Result<Vec<_>, _>>()?
                            .into_boxed_slice(),
                        result: resolved(selected.ret)?,
                        declared_result: selected.member.declared_ret.map(resolved).transpose()?,
                        suspend: selected.member.suspend(),
                        can_inline: selected.member.inline.can_inline(),
                        inline_plan: fir_inline_body_plan(
                            selected.member.inline_body_plan.as_deref(),
                        ),
                        extension_receiver_parameter: None,
                    }
                };
                let context_count = selected.context_args.len();
                let parameters = selected
                    .member
                    .params
                    .get(context_count..)
                    .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let context_parameters = selected.member.params[..context_count]
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                Ok(SelectedOperatorTarget {
                    target,
                    extension: false,
                    context_parameters,
                    value_parameters: parameters,
                    vararg_index: selected.member.call_sig.vararg_index,
                    context_arguments: selected.context_args.clone(),
                })
            }
            Some(ResolvedCall::Extension(selected)) => {
                let context_count = selected
                    .callable
                    .context_count
                    .min(selected.callable.params.len());
                let target = if let Some(declaration) = selected.stable_declaration {
                    self.index
                        .callable_for_declaration(declaration)
                        .map(|callable| FirCallTarget::Module(callable.id))
                        .ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?
                } else {
                    FirCallTarget::External {
                        declaration: selected.callable.external_identity.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?,
                        receiver: Some(resolved(selected.receiver)?),
                        declared_receiver: selected
                            .callable
                            .source_receiver
                            .map(resolved)
                            .transpose()?,
                        parameters: selected.callable.params[..context_count]
                            .iter()
                            .copied()
                            .chain(selected.params.iter().copied())
                            .map(resolved)
                            .collect::<Result<Vec<_>, _>>()?
                            .into_boxed_slice(),
                        result: resolved(selected.callable.ret)?,
                        declared_result: selected
                            .callable
                            .declared_ret
                            .map(resolved)
                            .transpose()?,
                        suspend: selected.callable.suspend,
                        can_inline: selected.callable.inline.can_inline(),
                        inline_plan: fir_inline_body_plan(
                            selected.callable.inline_body_plan.as_deref(),
                        ),
                        extension_receiver_parameter: None,
                    }
                };
                let parameters = selected
                    .params
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let context_parameters = selected.callable.params[..context_count]
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                Ok(SelectedOperatorTarget {
                    target,
                    extension: true,
                    context_parameters,
                    value_parameters: parameters,
                    vararg_index: selected.vararg_index,
                    context_arguments: selected.context_args.iter().cloned().map(Some).collect(),
                })
            }
            Some(ResolvedCall::MemberExtension {
                stable_declaration,
                external_identity,
                dispatch_receiver,
                extension_receiver,
                params,
                context_args,
                ret,
                declared_ret,
                suspend,
                inline,
                inline_body_plan,
                vararg_index,
                ..
            }) => {
                let context_count = context_args.len();
                let parameters = params
                    .get(context_count..)
                    .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let target = if let Some(declaration) = stable_declaration {
                    self.index
                        .callable_for_declaration(*declaration)
                        .map(|callable| FirCallTarget::Module(callable.id))
                        .ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?
                } else {
                    let extension_parameter = u32::try_from(context_count).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                    let mut target_parameters = params.clone();
                    target_parameters.insert(context_count, *extension_receiver);
                    FirCallTarget::External {
                        declaration: external_identity.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                        })?,
                        receiver: Some(resolved(dispatch_receiver.ty)?),
                        declared_receiver: None,
                        parameters: target_parameters
                            .into_iter()
                            .map(resolved)
                            .collect::<Result<Vec<_>, _>>()?
                            .into_boxed_slice(),
                        result: resolved(*ret)?,
                        declared_result: declared_ret.map(resolved).transpose()?,
                        suspend: *suspend,
                        can_inline: inline.can_inline(),
                        inline_plan: fir_inline_body_plan(inline_body_plan.as_deref()),
                        extension_receiver_parameter: Some(extension_parameter),
                    }
                };
                let context_parameters = params[..context_count]
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                Ok(SelectedOperatorTarget {
                    target,
                    extension: true,
                    context_parameters,
                    value_parameters: parameters,
                    vararg_index: *vararg_index,
                    context_arguments: context_args.clone(),
                })
            }
            Some(
                ResolvedCall::TopLevel(_)
                | ResolvedCall::Companion(_)
                | ResolvedCall::LocalFunction(_),
            )
            | None => Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)),
        }
    }

    pub(super) fn call_substitutions(
        &self,
        expression: ExprId,
        declaration: DeclarationId,
    ) -> Result<Box<[FirTypeSubstitution]>, BodyCheckFailure> {
        self.info
            .resolved_call_type_args
            .get(&expression)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ordinal, argument)| {
                let additional_bounds = self
                    .info
                    .resolved_call_type_argument_bounds
                    .get(&expression)
                    .and_then(|bounds| bounds.get(ordinal))
                    .into_iter()
                    .flatten()
                    .copied()
                    .map(|bound| {
                        ResolvedTy::new(bound).map_err(|error| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnpublishableType(error),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
                let parameter =
                    self.index
                        .type_parameter(declaration, ordinal)
                        .ok_or_else(|| {
                            crate::trace_compiler!(
                                "fir",
                                "missing type parameter declaration={declaration:?} ordinal={ordinal} name={:?}",
                                self.index.declaration_name(declaration),
                            );
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::MissingStableCallTarget,
                            )
                        })?;
                let value = argument.ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
                Ok(FirTypeSubstitution {
                    parameter: parameter.into(),
                    value: ResolvedTy::new(value).map_err(|error| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnpublishableType(error),
                        )
                    })?,
                    additional_bounds,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()
            .map(Vec::into_boxed_slice)
    }

    fn external_call_target(
        &self,
        expression: ExprId,
        callable: &crate::libraries::LibraryCallable,
        receiver: Option<Ty>,
        parameters: impl IntoIterator<Item = Ty>,
    ) -> Result<(FirCallTarget, Box<[FirTypeSubstitution]>), BodyCheckFailure> {
        let declaration = callable.external_identity.ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        self.external_call_target_with_identity(
            expression,
            declaration,
            receiver,
            callable.source_receiver,
            parameters,
            callable.ret,
            callable.declared_ret,
            callable.suspend,
            callable.inline.can_inline(),
            callable.inline_body_plan.as_deref(),
        )
    }

    /// Publish one already-selected member-extension call without branching on provider origin in
    /// lowering. Module declarations keep their callable identity. Dependency declarations carry
    /// the same semantic parameters plus the extension receiver's exact physical slot.
    pub(super) fn member_extension_call_target(
        &self,
        expression: ExprId,
        selected: &ResolvedCall,
    ) -> Result<MemberExtensionFirTarget, BodyCheckFailure> {
        let ResolvedCall::MemberExtension {
            stable_declaration,
            external_identity,
            owner,
            name,
            dispatch_receiver,
            extension_receiver,
            params,
            context_args,
            ret,
            inline,
            inline_body_plan,
            suspend,
            declared_ret,
            ..
        } = selected
        else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        crate::trace_compiler!(
            "fir",
            "publish member-extension expression={expression:?} span={:?} stable={stable_declaration:?} external={external_identity:?} owner={owner:?} name={name} dispatch={dispatch_receiver:?} extension={extension_receiver:?} params={params:?}",
            self.file.expr_span(expression),
        );
        let parameters = self.selected_call_parameters(expression, *stable_declaration, params)?;
        if let Some(declaration) = stable_declaration {
            let callable = self
                .index
                .callable_for_declaration(*declaration)
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            return Ok(MemberExtensionFirTarget {
                target: FirCallTarget::Module(callable.id),
                substitutions: self.call_substitutions(expression, *declaration)?,
                parameters,
                extension_parameter: None,
            });
        }

        let declaration = external_identity.ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "member-extension call has no stable target expression={expression:?} span={:?} owner={owner:?} name={name} receiver={extension_receiver:?} params={params:?}",
                self.file.expr_span(expression),
            );
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        let extension_parameter = context_args.len();
        if extension_parameter > parameters.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let mut target_parameters = parameters.clone();
        target_parameters.insert(extension_parameter, *extension_receiver);
        let (mut target, substitutions) = self.external_call_target_with_identity(
            expression,
            declaration,
            Some(dispatch_receiver.ty),
            None,
            target_parameters,
            *ret,
            *declared_ret,
            *suspend,
            inline.can_inline(),
            inline_body_plan.as_deref(),
        )?;
        let extension_parameter = u32::try_from(extension_parameter).map_err(|_| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        let FirCallTarget::External {
            extension_receiver_parameter,
            ..
        } = &mut target
        else {
            unreachable!("an external member extension must produce an external FIR target")
        };
        *extension_receiver_parameter = Some(extension_parameter);
        Ok(MemberExtensionFirTarget {
            target,
            substitutions,
            parameters,
            extension_parameter: Some(extension_parameter),
        })
    }

    pub(super) fn member_extension_arguments(
        &self,
        expression: ExprId,
        mut arguments: Box<[FirCallArgument]>,
        extension_parameter: Option<u32>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        let Some(extension_parameter) = extension_parameter else {
            return Ok(arguments);
        };
        for argument in &mut arguments {
            let parameter = match argument {
                FirCallArgument::Expression { parameter, .. }
                | FirCallArgument::Default { parameter, .. }
                | FirCallArgument::Vararg { parameter, .. } => parameter,
            };
            if *parameter >= extension_parameter {
                *parameter = parameter.checked_add(1).ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
            }
        }
        Ok(arguments)
    }

    fn external_call_target_with_identity(
        &self,
        expression: ExprId,
        declaration: ExternalCallableId,
        receiver: Option<Ty>,
        declared_receiver: Option<Ty>,
        parameters: impl IntoIterator<Item = Ty>,
        result: Ty,
        declared_result: Option<Ty>,
        suspend: bool,
        can_inline: bool,
        inline_plan: Option<&crate::libraries::InlineBodyPlan>,
    ) -> Result<(FirCallTarget, Box<[FirTypeSubstitution]>), BodyCheckFailure> {
        let resolved = |ty| {
            ResolvedTy::new(ty).map_err(|error| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnpublishableType(error),
                )
            })
        };
        let parameters = parameters
            .into_iter()
            .map(resolved)
            .collect::<Result<Vec<_>, _>>()?;
        let substitutions = self
            .info
            .resolved_call_type_args
            .get(&expression)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ordinal, argument)| {
                let additional_bounds = self
                    .info
                    .resolved_call_type_argument_bounds
                    .get(&expression)
                    .and_then(|bounds| bounds.get(ordinal))
                    .into_iter()
                    .flatten()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
                let value = argument.ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
                Ok(FirTypeSubstitution {
                    parameter: FirTypeParameterRef::External {
                        callable: declaration,
                        ordinal,
                    },
                    value: resolved(value)?,
                    additional_bounds,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        Ok((
            FirCallTarget::External {
                declaration,
                receiver: receiver.map(resolved).transpose()?,
                declared_receiver: declared_receiver.map(resolved).transpose()?,
                parameters: parameters.clone().into_boxed_slice(),
                result: resolved(result)?,
                declared_result: declared_result.map(resolved).transpose()?,
                suspend,
                can_inline,
                inline_plan: fir_inline_body_plan(inline_plan),
                extension_receiver_parameter: None,
            },
            substitutions.into_boxed_slice(),
        ))
    }

    pub(super) fn top_level_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let selected = self
            .info
            .resolved_top_level_call(expression)
            .cloned()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        self.selected_top_level_call(expression, arguments, selected)
    }

    fn selected_top_level_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        selected: crate::resolve::ResolvedTopLevelCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        crate::trace_compiler!(
            "fir",
            "selected top-level call expression={expression:?} declaration={:?} external={:?} suspend={} params={:?} result={:?} type_args={:?}",
            selected.stable_declaration,
            selected.callable.external_identity,
            selected.callable.suspend,
            selected.callable.params,
            selected.callable.ret,
            self.info.resolved_call_type_args.get(&expression),
        );
        let dispatch_receiver = selected
            .callable
            .singleton_dispatch
            .as_deref()
            .map(|singleton| self.singleton_call_receiver(expression, singleton))
            .transpose()?;
        let dispatch_ty = selected
            .callable
            .singleton_dispatch
            .as_deref()
            .map(|singleton| singleton.ty());
        let selected_intrinsic = match selected.callable.compiler_intrinsic {
            Some(crate::libraries::CompilerIntrinsic::SuspendCoroutine) => {
                Some(FirIntrinsic::SuspendCoroutine)
            }
            Some(crate::libraries::CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn) => {
                Some(FirIntrinsic::SuspendCoroutineUninterceptedOrReturn)
            }
            _ => None,
        };
        let (target, substitutions) = if let Some(operation) = selected_intrinsic {
            if dispatch_receiver.is_some() {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
            let span = self.file.expr_span(expression);
            let resolved = |ty| {
                ResolvedTy::new(ty).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            };
            (
                FirCallTarget::Intrinsic {
                    operation,
                    receiver: None,
                    parameters: selected
                        .callable
                        .params
                        .iter()
                        .copied()
                        .map(resolved)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_boxed_slice(),
                    result: resolved(selected.callable.ret)?,
                },
                Vec::<FirTypeSubstitution>::new().into_boxed_slice(),
            )
        } else if let Some(declaration) = selected.stable_declaration {
            let callable = self
                .index
                .callable_for_declaration(declaration)
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            (
                FirCallTarget::Module(callable.id),
                self.call_substitutions(expression, declaration)?,
            )
        } else {
            self.external_call_target(
                expression,
                &selected.callable,
                dispatch_ty,
                selected.callable.params.iter().copied(),
            )?
        };
        let parameters = self.selected_call_parameters(
            expression,
            selected.stable_declaration,
            &selected.callable.params,
        )?;
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver,
            extension_receiver: None,
            parameter_types: self
                .published_parameter_types(self.file.expr_span(expression), &parameters)?,
            arguments: self.call_arguments_with_context(
                expression,
                arguments,
                &parameters,
                selected.context_args.iter().map(Option::as_ref),
                selected.vararg_index,
            )?,
            substitutions,
        }))
    }

    pub(super) fn member_call(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let selected = self
            .info
            .resolved_member(expression)
            .cloned()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        let receiver = self.explicit_receiver(receiver)?;
        self.selected_member_call_with_semantics(expression, arguments, &selected, receiver)
    }

    /// Publish compiler-supplied member realizations from the exact selected declaration. Explicit
    /// and implicit receiver syntax must converge here; neither the callable spelling nor the
    /// backend is allowed to rediscover whether the declaration is an intrinsic or range builder.
    fn selected_member_call_with_semantics(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        selected: &crate::symbol_resolver::ResolvedMember,
        receiver: FirReceiver,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if let crate::libraries::MemberRealization::RangeConstruction { open_end } =
            selected.member.realization
        {
            let [end] = arguments else {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            };
            return Ok(FirExprKind::Range {
                operation: if open_end {
                    FirRangeOperation::OpenEnd
                } else {
                    FirRangeOperation::Through
                },
                start: receiver.value,
                start_type: self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    selected.receiver,
                )?,
                end: self.expression(*end)?,
                end_type: self.resolved_type(
                    self.file.expr_span(*end).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    selected.member.params[0],
                )?,
            });
        }
        let primitive_operation = match selected.member.realization {
            crate::libraries::MemberRealization::Intrinsic(intrinsic) => Some(intrinsic),
            crate::libraries::MemberRealization::Dispatch
            | crate::libraries::MemberRealization::Direct { .. }
            | crate::libraries::MemberRealization::RangeConstruction { .. } => None,
        };
        if let Some(operation) = primitive_operation.and_then(intrinsic_binary_operation) {
            let [argument] = arguments else {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            };
            return Ok(FirExprKind::Binary {
                operation,
                lhs: receiver.value,
                rhs: self.expression(*argument)?,
            });
        }
        if primitive_operation == Some(crate::libraries::CompilerIntrinsic::PrimitiveCompare) {
            return self.primitive_compare_call(expression, receiver, arguments, &selected);
        }
        if primitive_operation == Some(crate::libraries::CompilerIntrinsic::PrimitiveBitNot) {
            if !arguments.is_empty() {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
            return Ok(FirExprKind::Unary {
                operation: FirUnaryOperation::BitwiseNot,
                operand: receiver.value,
            });
        }
        if primitive_operation == Some(crate::libraries::CompilerIntrinsic::BooleanNot) {
            if !arguments.is_empty() {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
            return Ok(FirExprKind::Unary {
                operation: FirUnaryOperation::BooleanNot,
                operand: receiver.value,
            });
        }
        if primitive_operation == Some(crate::libraries::CompilerIntrinsic::NullableAnyToString) {
            if !arguments.is_empty() {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
            let resolved = |ty| {
                ResolvedTy::new(ty).map_err(|error| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnpublishableType(error),
                    )
                })
            };
            return Ok(FirExprKind::Call(FirCall {
                target: FirCallTarget::Intrinsic {
                    operation: FirIntrinsic::NullableAnyToString,
                    receiver: Some(resolved(selected.receiver)?),
                    parameters: Box::new([]),
                    result: resolved(selected.ret)?,
                },
                dispatch_receiver: Some(receiver),
                extension_receiver: None,
                parameter_types: Box::new([]),
                arguments: Box::new([]),
                substitutions: Box::new([]),
            }));
        }
        if selected.member.realization
            == crate::libraries::MemberRealization::Intrinsic(
                crate::libraries::CompilerIntrinsic::NumericConversion,
            )
        {
            if !arguments.is_empty() {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
            let cause = self.expression_origin(expression)?;
            let target = ResolvedTy::new(selected.ret).map_err(|error| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnpublishableType(error),
                )
            })?;
            return Ok(FirExprKind::ImplicitConversion {
                value: receiver.value,
                conversion: FirConversion {
                    origin: cause,
                    kind: FirConversionKind::NumericConversion { to: target },
                },
            });
        }
        self.selected_member_call_with_receiver(expression, arguments, &selected, receiver)
    }

    /// Publish an exact member callable after another checked source construct (notably an
    /// accessor-derived property) has already materialized its dispatch receiver. Selection is
    /// complete; this only converts the selected callable and argument mapping into stable FIR.
    pub(super) fn selected_member_call_with_receiver(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        selected: &crate::symbol_resolver::ResolvedMember,
        receiver: FirReceiver,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let (target, substitutions) = self.member_call_target(expression, selected)?;
        let parameters = self.selected_call_parameters(
            expression,
            selected.member.stable_declaration,
            &selected.member.params,
        )?;
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            parameter_types: self
                .published_parameter_types(self.file.expr_span(expression), &parameters)?,
            arguments: self.call_arguments_with_context(
                expression,
                arguments,
                &parameters,
                selected.context_args.iter().map(Option::as_ref),
                selected.member.call_sig.vararg_index,
            )?,
            substitutions,
        }))
    }

    /// Statement-form property setter call. There is no source call expression to use for argument
    /// mapping: `value` is the setter's argument, not the call site itself. Build the already-known
    /// context/value slots directly so checking the value cannot recurse into itself.
    pub(super) fn selected_member_setter_call(
        &mut self,
        value: ExprId,
        selected: &crate::symbol_resolver::ResolvedMember,
        receiver: FirReceiver,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let cause = self.expression_origin(value)?;
        let span = self.file.expr_span(value);
        let (target, substitutions) = self.member_call_target(value, selected)?;
        let parameters = self.selected_call_parameters(
            value,
            selected.member.stable_declaration,
            &selected.member.params,
        )?;
        let mut arguments = selected
            .context_args
            .iter()
            .enumerate()
            .map(|(parameter, argument)| {
                let argument = argument.as_ref().ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let context = self.materialize_context_argument(value, cause, argument)?;
                Ok(FirCallArgument::Expression {
                    parameter: u32::try_from(parameter).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                    value: context.value,
                    conversion: self.receiver_conversion(
                        value,
                        cause,
                        context,
                        parameters.get(parameter).copied(),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        let parameter = selected.context_args.len();
        let target_ty = parameters
            .get(parameter)
            .copied()
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        arguments.push(FirCallArgument::Expression {
            parameter: u32::try_from(parameter)
                .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?,
            value: self.expression(value)?,
            conversion: self.selected_value_conversion(
                value,
                ResolvedTy::new(target_ty).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?,
                cause,
            )?,
        });
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver: Some(receiver),
            extension_receiver: None,
            parameter_types: self.published_parameter_types(span, &parameters)?,
            arguments: arguments.into_boxed_slice(),
            substitutions,
        }))
    }

    pub(super) fn member_call_target(
        &self,
        expression: ExprId,
        selected: &crate::symbol_resolver::ResolvedMember,
    ) -> Result<(FirCallTarget, Box<[FirTypeSubstitution]>), BodyCheckFailure> {
        if let Some(declaration) = selected.member.stable_declaration {
            crate::trace_compiler!(
                "fir",
                "publish member call expression={expression:?} declaration={declaration:?} name={} type_args={:?}",
                selected.member.name,
                self.info.resolved_call_type_args.get(&expression),
            );
            let callable = self
                .index
                .callable_for_declaration(declaration)
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            return Ok((
                FirCallTarget::Module(callable.id),
                self.call_substitutions(expression, declaration)?,
            ));
        }
        let declaration = selected.member.external_identity.ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        crate::trace_compiler!(
            "fir",
            "checked external member target id={declaration:?} owner={:?} name={} params={:?} result={:?}",
            selected.member.owner,
            selected.member.name,
            selected.member.params,
            selected.ret,
        );
        self.external_call_target_with_identity(
            expression,
            declaration,
            Some(selected.receiver),
            None,
            selected.member.params.iter().copied(),
            selected.ret,
            selected.member.declared_ret,
            selected.member.suspend(),
            selected.member.inline.can_inline(),
            selected.member.inline_body_plan.as_deref(),
        )
    }

    pub(super) fn source_member_operator_call(
        &mut self,
        expression: ExprId,
        convention: &str,
        receiver: ExprId,
        operands: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let selected = self
            .info
            .resolved_operator_call(expression, convention)
            .cloned()
            .or_else(|| {
                (convention == "get")
                    .then(|| self.info.resolved_index_get_call(expression).cloned())
                    .flatten()
            });
        if let Some(ResolvedCall::Member(member)) = selected.as_ref() {
            if member.member.realization
                == crate::libraries::MemberRealization::Intrinsic(
                    crate::libraries::CompilerIntrinsic::PrimitiveCompare,
                )
            {
                let receiver = self.explicit_receiver(receiver)?;
                return self.primitive_compare_call(expression, receiver, operands, member);
            }
        }
        let dispatch_receiver = self.expression(receiver)?;
        if let Some(ResolvedCall::LocalFunction(selected)) = selected {
            let cause = self.expression_origin(expression)?;
            return self.local_operator_call_on_value(
                self.file.expr_span(expression),
                cause,
                &selected,
                dispatch_receiver,
                operands,
            );
        }
        self.source_member_operator_call_on_value(
            expression,
            convention,
            dispatch_receiver,
            operands,
        )
        .map(FirExprKind::Call)
    }

    pub(super) fn source_member_operator_call_on_value(
        &mut self,
        expression: ExprId,
        convention: &str,
        explicit_receiver: FirExprId,
        operands: &[ExprId],
    ) -> Result<FirCall, BodyCheckFailure> {
        if let Some(
            ref selected @ ResolvedCall::MemberExtension {
                ref dispatch_receiver,
                ref context_args,
                vararg_index,
                ..
            },
        ) = self
            .info
            .resolved_operator_call(expression, convention)
            .cloned()
            .or_else(|| {
                (convention == "get")
                    .then(|| self.info.resolved_index_get_call(expression).cloned())
                    .flatten()
            })
        {
            let target = self.member_extension_call_target(expression, &selected)?;
            let cause = self.expression_origin(expression)?;
            let dispatch_receiver = self
                .materialize_implicit_receiver(
                    cause,
                    self.file.expr_span(expression),
                    dispatch_receiver,
                )?
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?;
            let arguments = self.call_arguments_with_context(
                expression,
                operands,
                &target.parameters,
                context_args.iter().map(Option::as_ref),
                vararg_index,
            )?;
            let parameter_types = self
                .published_parameter_types(self.file.expr_span(expression), &target.parameters)?;
            return Ok(FirCall {
                target: target.target,
                dispatch_receiver: Some(dispatch_receiver),
                extension_receiver: Some(FirReceiver {
                    value: explicit_receiver,
                    conversion: None,
                }),
                parameter_types,
                arguments: self.member_extension_arguments(
                    expression,
                    arguments,
                    target.extension_parameter,
                )?,
                substitutions: target.substitutions,
            });
        }
        let (target, substitutions, extension, context_args, vararg_index, parameters) =
            self.expression_member_operator_target(expression, convention)?;
        let arguments = self.call_arguments_with_context(
            expression,
            operands,
            &parameters,
            context_args.iter().map(Option::as_ref),
            vararg_index,
        )?;
        let receiver = FirReceiver {
            value: explicit_receiver,
            conversion: None,
        };
        Ok(FirCall {
            target,
            dispatch_receiver: (!extension).then_some(receiver),
            extension_receiver: extension.then_some(receiver),
            parameter_types: self
                .published_parameter_types(self.file.expr_span(expression), &parameters)?,
            arguments,
            substitutions,
        })
    }

    pub(super) fn source_member_statement_operator_call(
        &mut self,
        statement: StmtId,
        convention: &str,
        receiver: ExprId,
        operands: &[ExprId],
    ) -> Result<FirCall, BodyCheckFailure> {
        let member_extension_dispatch =
            match self.info.resolved_stmt_operator_call(statement, convention) {
                Some(ResolvedCall::MemberExtension {
                    dispatch_receiver, ..
                }) => Some(dispatch_receiver.clone()),
                _ => None,
            };
        let selected = self.statement_operator_target(statement, convention)?;
        let parameter_types = selected.parameter_types();
        let cause = self.statement_origin(statement)?;
        let mut arguments = selected
            .context_arguments
            .iter()
            .enumerate()
            .map(|(parameter, argument)| {
                let receiver = self.materialize_context_argument(
                    receiver,
                    cause,
                    argument.as_ref().ok_or_else(|| {
                        self.failure(
                            self.file.stmt_spans.get(statement.0 as usize).copied(),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?,
                )?;
                Ok(FirCallArgument::Expression {
                    parameter: parameter as u32,
                    value: receiver.value,
                    conversion: receiver.conversion,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        let slots = self
            .info
            .resolved_stmt_operator_arg_slots(statement, convention)
            .map(<[_]>::to_vec);
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let mut saw_vararg = false;
        for (source, operand) in operands.iter().copied().enumerate() {
            let parameter = slots
                .as_deref()
                .and_then(|slots| slots.iter().position(|slot| *slot == Some(operand)))
                .or_else(|| selected.vararg_index.filter(|vararg| source >= *vararg))
                .unwrap_or(source);
            let parameter_ty = selected
                .value_parameters
                .get(parameter)
                .copied()
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            let parameter_id = u32::try_from(parameter + selected.context_arguments.len())
                .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            let value = self.expression(operand)?;
            if selected.vararg_index != Some(parameter) {
                arguments.push(FirCallArgument::Expression {
                    parameter: parameter_id,
                    value,
                    conversion: self.selected_value_conversion(operand, parameter_ty, cause)?,
                });
                continue;
            }
            saw_vararg = true;
            if self
                .info
                .resolved_whole_array_vararg_args
                .contains(&operand)
            {
                arguments.push(FirCallArgument::Expression {
                    parameter: parameter_id,
                    value,
                    conversion: self.selected_value_conversion(operand, parameter_ty, cause)?,
                });
                continue;
            }
            let expected = if self.file.is_spread_arg(operand) {
                parameter_ty
            } else {
                let element = parameter_ty.get().array_elem().ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                self.resolved_type(
                    span.ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    element,
                )?
            };
            arguments.push(FirCallArgument::Vararg {
                parameter: parameter_id,
                origin: cause,
                elements: vec![FirVarargElement {
                    value,
                    spread: self.file.is_spread_arg(operand),
                    conversion: self.selected_value_conversion(operand, expected, cause)?,
                }]
                .into_boxed_slice(),
            });
        }
        if let Some(parameter) = selected.vararg_index.filter(|_| !saw_vararg) {
            arguments.push(FirCallArgument::Vararg {
                parameter: u32::try_from(parameter + selected.context_arguments.len())
                    .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?,
                origin: self
                    .origins
                    .synthetic(cause, SyntheticOriginKind::VarargArray),
                elements: Box::new([]),
            });
        }
        if let Some(slots) = slots {
            for (parameter, slot) in slots.iter().enumerate() {
                if slot.is_none() && selected.vararg_index != Some(parameter) {
                    arguments.push(FirCallArgument::Default {
                        parameter: u32::try_from(parameter + selected.context_arguments.len())
                            .map_err(|_| {
                                self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                            })?,
                        origin: self
                            .origins
                            .synthetic(cause, SyntheticOriginKind::DefaultArgument),
                    });
                }
            }
        }
        let receiver = FirReceiver {
            value: self.expression(receiver)?,
            conversion: None,
        };
        if let Some(dispatch) = member_extension_dispatch {
            let extension_parameter = match &selected.target {
                FirCallTarget::External {
                    extension_receiver_parameter,
                    ..
                } => *extension_receiver_parameter,
                FirCallTarget::Module(_)
                | FirCallTarget::Intrinsic { .. }
                | FirCallTarget::Classifier { .. }
                | FirCallTarget::Super { .. } => None,
            };
            if let Some(extension_parameter) = extension_parameter {
                for argument in &mut arguments {
                    let parameter = match argument {
                        FirCallArgument::Expression { parameter, .. }
                        | FirCallArgument::Default { parameter, .. }
                        | FirCallArgument::Vararg { parameter, .. } => parameter,
                    };
                    if *parameter >= extension_parameter {
                        *parameter = parameter.checked_add(1).ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                    }
                }
            }
            let dispatch_receiver = self
                .materialize_implicit_receiver(cause, span, &dispatch)?
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            return Ok(FirCall {
                target: selected.target,
                dispatch_receiver: Some(dispatch_receiver),
                extension_receiver: Some(receiver),
                parameter_types,
                arguments: arguments.into_boxed_slice(),
                substitutions: Box::new([]),
            });
        }
        Ok(FirCall {
            target: selected.target,
            dispatch_receiver: (!selected.extension).then_some(receiver),
            extension_receiver: selected.extension.then_some(receiver),
            parameter_types,
            arguments: arguments.into_boxed_slice(),
            substitutions: Box::new([]),
        })
    }

    pub(super) fn source_member_statement_operator_on_value(
        &mut self,
        statement: StmtId,
        convention: &str,
        receiver: FirExprId,
    ) -> Result<FirCall, BodyCheckFailure> {
        let member_extension_dispatch =
            match self.info.resolved_stmt_operator_call(statement, convention) {
                Some(ResolvedCall::MemberExtension {
                    dispatch_receiver, ..
                }) => Some(dispatch_receiver.clone()),
                _ => None,
            };
        let selected = self.statement_operator_target(statement, convention)?;
        let parameter_types = selected.parameter_types();
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        if selected.vararg_index.is_some() {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        }
        let cause = self.statement_origin(statement)?;
        let mut arguments = selected
            .context_arguments
            .iter()
            .enumerate()
            .map(|(parameter, argument)| {
                let argument = argument.as_ref().ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let receiver = self.materialize_context_argument_at(span, cause, argument)?;
                Ok(FirCallArgument::Expression {
                    parameter: u32::try_from(parameter).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                    value: receiver.value,
                    conversion: receiver.conversion,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        if !selected.value_parameters.is_empty() {
            let slots = self
                .info
                .resolved_stmt_operator_arg_slots(statement, convention)
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            if slots.len() != selected.value_parameters.len() || slots.iter().any(Option::is_some) {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            }
            arguments.extend(
                slots
                    .iter()
                    .enumerate()
                    .map(|(parameter, _)| {
                        Ok(FirCallArgument::Default {
                            parameter: u32::try_from(parameter + selected.context_arguments.len())
                                .map_err(|_| {
                                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                                })?,
                            origin: self
                                .origins
                                .synthetic(cause, SyntheticOriginKind::DefaultArgument),
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?,
            );
        }
        let receiver = FirReceiver {
            value: receiver,
            conversion: None,
        };
        if let Some(dispatch) = member_extension_dispatch {
            let extension_parameter = match &selected.target {
                FirCallTarget::External {
                    extension_receiver_parameter,
                    ..
                } => *extension_receiver_parameter,
                FirCallTarget::Module(_)
                | FirCallTarget::Intrinsic { .. }
                | FirCallTarget::Classifier { .. }
                | FirCallTarget::Super { .. } => None,
            };
            if let Some(extension_parameter) = extension_parameter {
                for argument in &mut arguments {
                    let parameter = match argument {
                        FirCallArgument::Expression { parameter, .. }
                        | FirCallArgument::Default { parameter, .. }
                        | FirCallArgument::Vararg { parameter, .. } => parameter,
                    };
                    if *parameter >= extension_parameter {
                        *parameter = parameter.checked_add(1).ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                    }
                }
            }
            let dispatch_receiver = self
                .materialize_implicit_receiver(cause, span, &dispatch)?
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            return Ok(FirCall {
                target: selected.target,
                dispatch_receiver: Some(dispatch_receiver),
                extension_receiver: Some(receiver),
                parameter_types,
                arguments: arguments.into_boxed_slice(),
                substitutions: Box::new([]),
            });
        }
        Ok(FirCall {
            target: selected.target,
            dispatch_receiver: (!selected.extension).then_some(receiver),
            extension_receiver: selected.extension.then_some(receiver),
            parameter_types,
            arguments: arguments.into_boxed_slice(),
            substitutions: Box::new([]),
        })
    }

    /// Build checked FIR for a selected zero-argument convention in expression position. Local
    /// extensions use the body's stable local-callable identity; all non-local targets keep the
    /// ordinary checked-call representation.
    pub(super) fn zero_arg_expression_operator_call(
        &mut self,
        expression: ExprId,
        convention: &str,
        receiver: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if let Some(ResolvedCall::LocalFunction(selected)) = self
            .info
            .resolved_operator_call(expression, convention)
            .cloned()
        {
            let value = self.expression(receiver)?;
            let cause = self.expression_origin(expression)?;
            return self.local_operator_call_on_value(
                self.file.expr_span(expression),
                cause,
                &selected,
                value,
                &[],
            );
        }
        self.source_member_operator_call(expression, convention, receiver, &[])
    }

    /// Statement-position counterpart of [`Self::zero_arg_expression_operator_call`]. The caller
    /// has already synthesized the storage read that becomes the extension receiver.
    pub(super) fn zero_arg_statement_operator_call_on_value(
        &mut self,
        statement: StmtId,
        convention: &str,
        receiver: FirExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if let Some(ResolvedCall::LocalFunction(selected)) = self
            .info
            .resolved_stmt_operator_call(statement, convention)
            .cloned()
        {
            let cause = self.statement_origin(statement)?;
            return self.local_operator_call_on_value(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                cause,
                &selected,
                receiver,
                &[],
            );
        }
        self.source_member_statement_operator_on_value(statement, convention, receiver)
            .map(FirExprKind::Call)
    }
}

impl BodyFirChecker<'_> {
    /// Check a `super`-qualified call against the supertype declaration the checker already selected.
    ///
    /// `super` is not a receiver expression: the recorded [`ImplicitReceiverSelection`] names which
    /// enclosing instance supplies `this` (a labeled `super@Outer` targets an outer one), and the
    /// callable is fixed to one supertype declaration, so dispatch must stay non-virtual.
    pub(super) fn selected_super_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        target: &crate::resolve::ResolvedSuperCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let cause = self.expression_origin(expression)?;
        self.selected_super_call_at(span, cause, Some(expression), arguments, target)
    }

    /// As [`Self::selected_super_call`], but for a site that is a STATEMENT rather than an
    /// expression (`super.p = v`, whose selected setter is an ordinary super call whose single
    /// argument is the assigned value).
    pub(super) fn selected_super_call_at(
        &mut self,
        span: Option<Span>,
        cause: OriginId,
        expression: Option<ExprId>,
        arguments: &[ExprId],
        target: &crate::resolve::ResolvedSuperCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let dispatch_receiver = self
            .materialize_implicit_receiver(cause, span, &target.receiver)?
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let parameters = target
            .params
            .iter()
            .copied()
            .map(|parameter| {
                ResolvedTy::new(parameter).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checked = match expression {
            Some(expression) => self
                .call_arguments(expression, arguments, &target.params)?
                .into_vec(),
            None => {
                if arguments.len() != target.params.len() {
                    return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
                }
                arguments
                    .iter()
                    .enumerate()
                    .map(|(parameter, argument)| {
                        Ok(FirCallArgument::Expression {
                            parameter: u32::try_from(parameter).map_err(|_| {
                                self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                            })?,
                            value: self.expression(*argument)?,
                            conversion: self.selected_value_conversion(
                                *argument,
                                parameters[parameter],
                                cause,
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?
            }
        };
        let result = ResolvedTy::new(target.ret)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        Ok(FirExprKind::Call(FirCall {
            target: FirCallTarget::Super {
                owner: target.owner,
                name: target.name.clone(),
                parameters: parameters.clone().into_boxed_slice(),
                result,
                interface: target.interface,
                descriptor: target.descriptor.clone(),
                physical_result: ResolvedTy::new(target.physical_ret).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?,
                source: target
                    .stable_declaration
                    .and_then(|declaration| self.index.callable_for_declaration(declaration))
                    .map(|callable| callable.id),
                source_member: target.source_member.clone(),
            },
            dispatch_receiver: Some(dispatch_receiver),
            extension_receiver: None,
            parameter_types: parameters.into_boxed_slice(),
            arguments: checked.into_boxed_slice(),
            substitutions: Box::new([]),
        }))
    }
}
