//! Checked callable-reference bindings with stable declaration identities.

use super::*;
use crate::resolve::{AdaptedRefArgument, CallableReferenceBinding, CallableReferenceTarget};

impl BodyFirChecker<'_> {
    fn reference_function_type(&self, expression: ExprId) -> Result<ResolvedTy, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let function = self
            .info
            .callable_reference_type(expression)
            .filter(|ty| matches!(ty.non_null(), Ty::Fun(_)))
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        ResolvedTy::new(function)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
    }

    fn reference_is_reflective(&self, expression: ExprId) -> bool {
        let semantic = self.info.semantic_ty(expression);
        let reflective = self.info.callable_reference_is_reflective(expression)
            || !matches!(semantic.non_null(), Ty::Fun(_));
        crate::trace_compiler!(
            "callable_ref",
            "FIR reference expression={expression:?} semantic={semantic:?} reflective={reflective}"
        );
        reflective
    }

    pub(super) fn callable_reference(
        &mut self,
        expression: ExprId,
        receiver: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        if let Some(ExprLowering::ClassLiteral { unbound }) =
            self.info.expr_lowers.get(&expression).cloned()
        {
            return match unbound {
                Some(classifier) => Ok(FirExprKind::ClassLiteral {
                    classifier: Some(self.resolved_type(
                        span.ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                        classifier,
                    )?),
                    value: None,
                }),
                None => Ok(FirExprKind::ClassLiteral {
                    classifier: None,
                    value: Some(self.expression(receiver.ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?)?),
                }),
            };
        }
        if let Some(ExprLowering::FunctionInvokeReference { target, signature }) =
            self.info.expr_lowers.get(&expression).cloned()
        {
            let (Ty::Fun(target), Ty::Fun(reference)) = (target.non_null(), signature.non_null())
            else {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            };
            let callee = receiver
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            let callee = self.expression(callee)?;
            let mut resolved = |ty| {
                ResolvedTy::new(ty).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            };
            return Ok(FirExprKind::FunctionInvokeReference {
                callee,
                target_parameters: target
                    .params
                    .iter()
                    .copied()
                    .map(&mut resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                target_result: resolved(target.ret)?,
                target_suspend: target.suspend,
                reference_parameters: reference
                    .params
                    .iter()
                    .copied()
                    .map(&mut resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                reference_result: resolved(reference.ret)?,
                suspend: reference.suspend,
            });
        }
        if let Some(ExprLowering::TopLevelFunctionRef(reference)) =
            self.info.expr_lowers.get(&expression).cloned()
        {
            return if let Some(declaration) = reference.stable_declaration {
                self.reference_to_declaration(
                    expression,
                    declaration,
                    FirCallableReferenceBinding::Static,
                    None,
                    None,
                    None,
                )
            } else {
                self.reference_to_external(
                    expression,
                    reference.target.external_identity,
                    reference.target.external_default_provider,
                    reference.target.compiler_intrinsic,
                    None,
                    false,
                    &reference.target.params,
                    reference.target.ret,
                    FirCallableReferenceBinding::Static,
                    None,
                    None,
                    None,
                )
            };
        }

        if let Some(ExprLowering::SamConstructorReference {
            signature,
            internal,
            method,
            params,
            ret,
            declared_params,
            declared_ret,
            context_count,
            has_receiver,
            suspend,
        }) = self.info.expr_lowers.get(&expression).cloned()
        {
            let Ty::Fun(function) = signature.non_null() else {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            };
            let mut resolved = |ty| {
                ResolvedTy::new(ty).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            };
            let conversion = FirSamConversion {
                classifier: internal,
                method: method.into_boxed_str(),
                parameters: params
                    .into_iter()
                    .map(&mut resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                result: resolved(ret)?,
                declared_parameters: declared_params
                    .into_iter()
                    .map(&mut resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                declared_result: resolved(declared_ret)?,
                context_count: u32::try_from(context_count)
                    .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?,
                has_receiver,
                suspend,
                nullable: false,
            };
            let parameters = function
                .params
                .iter()
                .copied()
                .map(&mut resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();
            return Ok(FirExprKind::CallableReference {
                target: FirCallableReferenceTarget::Classifier {
                    classifier: internal,
                    operation: crate::fir::FirClassifierCallable::SamConstructor {
                        conversion: Box::new(conversion),
                    },
                    parameters,
                    result: resolved(function.ret)?,
                },
                function_type: self.reference_function_type(expression)?,
                reflective: self.reference_is_reflective(expression),
                binding: FirCallableReferenceBinding::Static,
                dispatch_receiver: None,
                extension_receiver: None,
                substitutions: Box::new([]),
                adaptation: None,
            });
        }

        if let Some(ExprLowering::AdaptedRef {
            target,
            stable_declaration,
            adapted_params,
            ret,
            resolved_argument_mapping,
            suspend_conversion,
            ..
        }) = self.info.expr_lowers.get(&expression).cloned()
        {
            let adaptation = self.reference_adaptation(
                expression,
                resolved_argument_mapping,
                &adapted_params,
                ret,
                suspend_conversion,
            )?;
            return if let Some(declaration) = stable_declaration {
                self.reference_to_declaration(
                    expression,
                    declaration,
                    FirCallableReferenceBinding::Static,
                    None,
                    None,
                    Some(adaptation),
                )
            } else {
                self.reference_to_external(
                    expression,
                    target.external_identity,
                    target.external_default_provider,
                    target.compiler_intrinsic,
                    None,
                    false,
                    &target.params,
                    target.ret,
                    FirCallableReferenceBinding::Static,
                    None,
                    None,
                    Some(adaptation),
                )
            };
        }

        if let Some(ExprLowering::AdaptedCallableReference {
            binding,
            target,
            argument_mapping,
            signature,
        }) = self.info.expr_lowers.get(&expression).cloned()
        {
            let Ty::Fun(function) = signature.non_null() else {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            };
            // A reference whose EXPRESSION type is reflective (`KFunction1<…>`) needs a real
            // callable-reference object, not a lambda: `kotlin.reflect.KFunction` is not a function
            // interface and the adapter would fail its cast at run time. Only a reference used as a
            // plain function value is materialized here.
            if !matches!(self.info.semantic_ty(expression).non_null(), Ty::Fun(_)) {
                return Err(self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::CallableRef),
                ));
            }
            let adaptation = self.reference_adaptation(
                expression,
                argument_mapping,
                &function.params,
                function.ret,
                false,
            )?;
            return self.selected_reference(
                expression,
                receiver,
                binding,
                target,
                Some(adaptation),
            );
        }

        if let Some(ExprLowering::AdaptedLocalFunctionRef {
            stmt_id,
            bound_receiver,
            argument_mapping,
            signature,
            suspend_conversion,
        }) = self.info.expr_lowers.get(&expression).cloned()
        {
            let origin = self.expression_origin(expression)?;
            let Ty::Fun(function) = signature.non_null() else {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            };
            let target = self
                .local_callable_ref(stmt_id, origin)?
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
            let adaptation = self.reference_adaptation(
                expression,
                argument_mapping,
                &function.params,
                function.ret,
                suspend_conversion,
            )?;
            return Ok(FirExprKind::LocalCallableReference {
                target,
                function_type: self.reference_function_type(expression)?,
                reflective: self.reference_is_reflective(expression),
                extension_receiver: bound_receiver
                    .then(|| {
                        let receiver = receiver.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                        Ok(FirReceiver {
                            value: self.expression(receiver)?,
                            conversion: None,
                        })
                    })
                    .transpose()?,
                adaptation: Some(Box::new(adaptation)),
            });
        }

        if let Some(ExprLowering::LocalFunction {
            stmt_id,
            bound_receiver,
        }) = self.info.expr_lowers.get(&expression).cloned()
        {
            let origin = self.expression_origin(expression)?;
            let target = self
                .local_callable_ref(stmt_id, origin)?
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
            return Ok(FirExprKind::LocalCallableReference {
                target,
                function_type: self.reference_function_type(expression)?,
                reflective: self.reference_is_reflective(expression),
                extension_receiver: bound_receiver
                    .then(|| {
                        let receiver = receiver.ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                        Ok(FirReceiver {
                            value: self.expression(receiver)?,
                            conversion: None,
                        })
                    })
                    .transpose()?,
                adaptation: None,
            });
        }

        if let Some(ExprLowering::ConstructorRef {
            internal: constructor_owner,
            constructor,
            outer,
            argument_mapping,
            signature,
            ..
        }) = self.info.expr_lowers.get(&expression).cloned()
        {
            let Ty::Fun(function) = signature.non_null() else {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            };
            let reference_parameters = function
                .params
                .iter()
                .copied()
                .map(ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?;
            let result = ResolvedTy::new(function.ret).map_err(|error| {
                self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
            })?;
            if constructor.stable_declaration.is_none()
                && matches!(&outer, crate::resolve::ConstructorReferenceOuter::Unbound)
                && argument_mapping.is_none()
            {
                if let Some(element) = function.ret.array_elem() {
                    return Ok(FirExprKind::CallableReference {
                        target: FirCallableReferenceTarget::Classifier {
                            classifier: constructor_owner,
                            operation: crate::fir::FirClassifierCallable::ArrayConstructor {
                                element: ResolvedTy::new(element).map_err(|error| {
                                    self.failure(
                                        span,
                                        BodyCheckFailureKind::UnpublishableType(error),
                                    )
                                })?,
                            },
                            parameters: reference_parameters.into_boxed_slice(),
                            result,
                        },
                        function_type: self.reference_function_type(expression)?,
                        reflective: self.reference_is_reflective(expression),
                        binding: FirCallableReferenceBinding::Static,
                        dispatch_receiver: None,
                        extension_receiver: None,
                        substitutions: Box::new([]),
                        adaptation: None,
                    });
                }
            }
            // Some source constructor candidates still lack their stable declaration coordinate.
            // Recover the already-selected declaration structurally under the resolved classifier:
            // Kotlin forbids two constructors with the same semantic parameter signature, so this
            // yields at most one identity and also covers classes with secondary constructors only.
            let classifier_declaration = self.index.classifier_declaration(constructor_owner);
            let indexed_constructor = classifier_declaration.and_then(|classifier| {
                let matches = (0..self.index.declaration_count())
                    .filter_map(|raw| {
                        let candidate = crate::fir::DeclarationId::from_raw(
                            u32::try_from(raw).expect("too many stable declarations"),
                        );
                        let anchor = self.index.declaration_anchor(candidate)?;
                        let signature = self.index.signature(candidate)?;
                        (anchor.kind == crate::fir::DeclarationKind::Constructor
                            && anchor.owner == Some(classifier)
                            && signature
                                .parameters
                                .iter()
                                .map(|parameter| parameter.get())
                                .eq(constructor.params.iter().copied()))
                        .then_some(candidate)
                    })
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [declaration] => Some(*declaration),
                    [] => {
                        // Generic specialization changes the selected provider member's parameter
                        // types (`Wrapper<T>(T)` becomes `Wrapper<String>(String)`), while the stable
                        // declaration signature intentionally remains symbolic. If this classifier
                        // declares exactly one constructor, its identity is nevertheless exact and
                        // no overload selection is being repeated here.
                        let constructors = (0..self.index.declaration_count())
                            .filter_map(|raw| {
                                let candidate = crate::fir::DeclarationId::from_raw(
                                    u32::try_from(raw).expect("too many stable declarations"),
                                );
                                let anchor = self.index.declaration_anchor(candidate)?;
                                (anchor.kind == crate::fir::DeclarationKind::Constructor
                                    && anchor.owner == Some(classifier))
                                .then_some(candidate)
                            })
                            .collect::<Vec<_>>();
                        match constructors.as_slice() {
                            [declaration] => Some(*declaration),
                            [] | [_, _, ..] => None,
                        }
                    }
                    [_, _, ..] => None,
                }
            });
            let target_parameters = constructor
                .params
                .iter()
                .copied()
                .map(ResolvedTy::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?;
            let target = if let Some(declaration) =
                constructor.stable_declaration.or(indexed_constructor)
            {
                let callable = self
                    .index
                    .callable_for_declaration(declaration)
                    .ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                crate::fir::FirConstructorTarget::Module(callable.id)
            } else if let Some(declaration) = constructor.external_identity {
                crate::fir::FirConstructorTarget::External {
                    declaration,
                    classifier: constructor_owner,
                    parameters: target_parameters.clone().into_boxed_slice(),
                    annotation: None,
                }
            } else {
                crate::trace_compiler!(
                    "fir",
                    "constructor reference has no declaration identity: stable={:?} external={:?} outer={:?} adaptation={}",
                    constructor.stable_declaration,
                    constructor.external_identity,
                    outer,
                    argument_mapping.is_some(),
                );
                return Err(self.failure(
                    span,
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::CallableRef),
                ));
            };
            let classifier = constructor_owner;
            let outer_classifier = classifier_declaration
                .and_then(|classifier| {
                    self.index
                        .declaration_header(classifier)
                        .filter(|header| header.flags.has(crate::fir::DeclarationFlags::INNER))
                        .and_then(|header| header.owner)
                })
                .and_then(|outer| self.index.classifier_header(outer));
            let outer_type = outer_classifier
                .map(|_| match &outer {
                    // The selected reference signature owns the exact applied leading parameter.
                    // Rebuilding it from the outer classifier header erases type arguments fixed by
                    // a qualified inner/typealias LHS (`Foo<String>::InnerAlias`).
                    crate::resolve::ConstructorReferenceOuter::Unbound => {
                        function.params.first().copied().ok_or_else(|| {
                            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                        })
                    }
                    crate::resolve::ConstructorReferenceOuter::Expression(receiver) => {
                        self.expression_type(*receiver).map(ResolvedTy::get)
                    }
                    crate::resolve::ConstructorReferenceOuter::Implicit(selection) => {
                        Ok(selection.ty)
                    }
                })
                .transpose()?
                .map(ResolvedTy::new)
                .transpose()
                .map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?;
            let adaptation = argument_mapping
                .map(|mut mapping| {
                    if outer_type.is_some()
                        && matches!(&outer, crate::resolve::ConstructorReferenceOuter::Unbound)
                    {
                        for argument in &mut mapping {
                            match argument {
                                AdaptedRefArgument::Value(value) => *value += 1,
                                AdaptedRefArgument::Vararg { values, .. } => {
                                    values.iter_mut().for_each(|value| *value += 1);
                                }
                                AdaptedRefArgument::Default => {}
                            }
                        }
                    }
                    self.reference_adaptation(
                        expression,
                        mapping,
                        &function.params,
                        function.ret,
                        false,
                    )
                })
                .transpose()?;
            let (binding, dispatch_receiver) = match outer {
                crate::resolve::ConstructorReferenceOuter::Unbound => {
                    (FirCallableReferenceBinding::Unbound, None)
                }
                crate::resolve::ConstructorReferenceOuter::Expression(receiver) => (
                    FirCallableReferenceBinding::Bound,
                    Some(FirReceiver {
                        value: self.expression(receiver)?,
                        conversion: None,
                    }),
                ),
                crate::resolve::ConstructorReferenceOuter::Implicit(selection) => {
                    let cause = self.expression_origin(expression)?;
                    (
                        FirCallableReferenceBinding::Bound,
                        Some(
                            self.materialize_implicit_receiver(cause, span, &selection)?
                                .ok_or_else(|| {
                                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                                })?,
                        ),
                    )
                }
            };
            return Ok(FirExprKind::CallableReference {
                target: FirCallableReferenceTarget::Constructor {
                    target,
                    classifier,
                    outer: outer_type,
                    parameters: target_parameters.into_boxed_slice(),
                    result,
                },
                function_type: self.reference_function_type(expression)?,
                reflective: self.reference_is_reflective(expression),
                binding,
                dispatch_receiver,
                extension_receiver: None,
                substitutions: Box::new([]),
                adaptation: adaptation.map(Box::new),
            });
        }
        let Some(ExprLowering::CallableReference { binding, target }) =
            self.info.expr_lowers.get(&expression).cloned()
        else {
            return Err(self.failure(
                span,
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::CallableRef),
            ));
        };
        self.selected_reference(expression, receiver, binding, target, None)
    }

    fn selected_reference(
        &mut self,
        expression: ExprId,
        receiver: Option<ExprId>,
        binding: CallableReferenceBinding,
        target: CallableReferenceTarget,
        adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        match target {
            CallableReferenceTarget::Member {
                receiver: receiver_ty,
                member,
            } => {
                let (binding, dispatch_receiver) =
                    self.reference_receiver(expression, receiver, binding)?;
                if let Some(declaration) = member.stable_declaration {
                    self.reference_to_declaration(
                        expression,
                        declaration,
                        binding,
                        dispatch_receiver,
                        None,
                        adaptation,
                    )
                } else {
                    self.reference_to_external(
                        expression,
                        member.external_identity,
                        member.external_default_provider,
                        None,
                        Some(receiver_ty),
                        false,
                        &member.params,
                        member.ret,
                        binding,
                        dispatch_receiver,
                        None,
                        adaptation,
                    )
                }
            }
            CallableReferenceTarget::Extension {
                callable,
                stable_declaration,
                companion_extension,
                ..
            } => {
                let adaptation = self.extension_reference_adaptation(
                    expression,
                    &binding,
                    callable
                        .params
                        .len()
                        .saturating_sub(usize::from(!companion_extension)),
                    adaptation,
                )?;
                let (binding, extension_receiver) = if companion_extension {
                    (FirCallableReferenceBinding::Static, None)
                } else {
                    self.reference_receiver(expression, receiver, binding)?
                };
                if let Some(declaration) = stable_declaration {
                    self.reference_to_declaration(
                        expression,
                        declaration,
                        binding,
                        None,
                        extension_receiver,
                        adaptation,
                    )
                } else {
                    let declared_receiver = callable.params.first().copied().ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                    let (receiver_ty, extension_target, parameters) = if companion_extension {
                        (None, false, callable.physical_params.as_slice())
                    } else {
                        (
                            Some(declared_receiver),
                            true,
                            callable.params.get(1..).unwrap_or_default(),
                        )
                    };
                    self.reference_to_external(
                        expression,
                        callable.external_identity,
                        callable.external_default_provider,
                        callable.compiler_intrinsic,
                        receiver_ty,
                        extension_target,
                        parameters,
                        callable.ret,
                        binding,
                        None,
                        extension_receiver,
                        adaptation,
                    )
                }
            }
            CallableReferenceTarget::Classifier(member) => {
                if let Some(operation) = member.implicit_classifier_callable {
                    let classifier = member.owner.ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                    let operation = match operation {
                        crate::libraries::ImplicitClassifierCallable::EnumValues => {
                            crate::fir::FirClassifierCallable::EnumValues
                        }
                        crate::libraries::ImplicitClassifierCallable::EnumValueOf => {
                            crate::fir::FirClassifierCallable::EnumValueOf
                        }
                    };
                    let parameters = member
                        .params
                        .iter()
                        .copied()
                        .map(ResolvedTy::new)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| {
                            self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                        })?;
                    let result = ResolvedTy::new(member.ret).map_err(|error| {
                        self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                    })?;
                    return Ok(FirExprKind::CallableReference {
                        target: FirCallableReferenceTarget::Classifier {
                            classifier,
                            operation,
                            parameters: parameters.into_boxed_slice(),
                            result,
                        },
                        function_type: self.reference_function_type(expression)?,
                        reflective: self.reference_is_reflective(expression),
                        binding: FirCallableReferenceBinding::Static,
                        dispatch_receiver: None,
                        extension_receiver: None,
                        substitutions: Box::new([]),
                        adaptation: adaptation.map(Box::new),
                    });
                }
                if let Some(declaration) = member.stable_declaration {
                    self.reference_to_declaration(
                        expression,
                        declaration,
                        FirCallableReferenceBinding::Static,
                        None,
                        None,
                        adaptation,
                    )
                } else {
                    self.reference_to_external(
                        expression,
                        member.external_identity,
                        member.external_default_provider,
                        None,
                        None,
                        false,
                        &member.params,
                        member.ret,
                        FirCallableReferenceBinding::Static,
                        None,
                        None,
                        adaptation,
                    )
                }
            }
            CallableReferenceTarget::Property(property) => {
                crate::trace_compiler!(
                    "fir",
                    "property reference expression={expression:?} name={} stable={:?} reflection_owner={:?}",
                    property.name,
                    property.stable_declaration,
                    property.reflection_owner,
                );
                if let Some(declaration) = property.stable_declaration {
                    crate::trace_compiler!(
                        "fir",
                        "property reference stable target declaration={declaration:?} name={:?} indexed={:?} header={:?}",
                        self.index.declaration_name(declaration),
                        self.index.property_for_declaration(declaration),
                        self.index.declaration_header(declaration),
                    );
                }
                let (binding, receiver) = if property.companion_extension {
                    (FirCallableReferenceBinding::Static, None)
                } else {
                    self.reference_receiver(expression, receiver, binding)?
                };
                let (dispatch_receiver, extension_receiver) = if property.companion_extension {
                    (None, None)
                } else if property.extension_facade.is_some() {
                    (None, receiver)
                } else {
                    (receiver, None)
                };
                if let Some(declaration) = property.stable_declaration {
                    let target_is_extension =
                        property.extension_facade.is_some() && !property.companion_extension;
                    let receiver_type = if property.companion_extension {
                        None
                    } else if target_is_extension {
                        property.getter.params.first().copied()
                    } else {
                        Some(property.reflection_owner)
                    };
                    self.property_reference_to_declaration(
                        expression,
                        declaration,
                        binding,
                        dispatch_receiver,
                        extension_receiver,
                        property.setter.is_some(),
                        receiver_type,
                        target_is_extension,
                        property.prop_ty,
                        adaptation,
                    )
                } else {
                    self.property_reference_to_external(
                        expression,
                        binding,
                        dispatch_receiver,
                        extension_receiver,
                        &property.name,
                        (!property.companion_extension).then_some(property.reflection_owner),
                        property.extension_facade.is_some() && !property.companion_extension,
                        &property.getter,
                        property.setter.as_ref(),
                        property.prop_ty,
                        adaptation,
                    )
                }
            }
            CallableReferenceTarget::TopLevelProperty(property) => {
                if let Some(declaration) = property.stable_declaration {
                    self.property_reference_to_declaration(
                        expression,
                        declaration,
                        FirCallableReferenceBinding::Static,
                        None,
                        None,
                        property.setter.is_some(),
                        None,
                        false,
                        property.ty,
                        adaptation,
                    )
                } else {
                    self.property_reference_to_external(
                        expression,
                        FirCallableReferenceBinding::Static,
                        None,
                        None,
                        &property.name,
                        None,
                        false,
                        &property.getter,
                        property.setter.as_ref(),
                        property.ty,
                        adaptation,
                    )
                }
            }
            CallableReferenceTarget::ClassifierProperty {
                owner, property, ..
            } => self.classifier_property_reference(
                expression,
                owner,
                match property.operation {
                    crate::libraries::ImplicitClassifierProperty::EnumEntries => {
                        FirClassifierProperty::EnumEntries
                    }
                },
                property.ty,
                adaptation,
            ),
        }
    }

    /// Resolution models an unbound extension receiver as the leading pseudo-parameter while it
    /// compares the callable against an expected function type. Checked FIR models that receiver
    /// independently through [`FirCallableReferenceBinding::Unbound`], so its target-parameter plan
    /// must contain declaration value parameters only. Value ordinals deliberately remain in the
    /// adapter function's namespace: after removing `Value(0)` for the receiver, `Value(1)` still
    /// denotes the first value parameter of `Receiver.(P) -> R`.
    fn extension_reference_adaptation(
        &self,
        expression: ExprId,
        binding: &CallableReferenceBinding,
        target_parameter_count: usize,
        mut adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<Option<FirReferenceAdaptation>, BodyCheckFailure> {
        if !matches!(binding, CallableReferenceBinding::Unbound) {
            return Ok(adaptation);
        }
        let Some(plan) = adaptation.as_mut() else {
            return Ok(adaptation);
        };
        if plan.arguments.len() == target_parameter_count {
            return Ok(adaptation);
        }
        if plan.arguments.len() != target_parameter_count.saturating_add(1) {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let mut arguments = std::mem::take(&mut plan.arguments).into_vec();
        if arguments.first() != Some(&FirAdaptedReferenceArgument::Value(0)) {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        arguments.remove(0);
        plan.arguments = arguments.into_boxed_slice();
        Ok(adaptation)
    }

    fn reference_receiver(
        &mut self,
        expression: ExprId,
        receiver: Option<ExprId>,
        binding: CallableReferenceBinding,
    ) -> Result<(FirCallableReferenceBinding, Option<FirReceiver>), BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        match binding {
            CallableReferenceBinding::Bound => {
                let receiver = receiver.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                Ok((
                    FirCallableReferenceBinding::Bound,
                    Some(FirReceiver {
                        value: self.expression(receiver)?,
                        conversion: None,
                    }),
                ))
            }
            CallableReferenceBinding::Unbound => Ok((FirCallableReferenceBinding::Unbound, None)),
            CallableReferenceBinding::ImplicitThis => Ok((
                FirCallableReferenceBinding::Bound,
                Some(self.implicit_receiver(expression)?.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?),
            )),
            CallableReferenceBinding::Singleton(singleton) => {
                let cause = self.expression_origin(expression)?;
                let origin = self
                    .origins
                    .synthetic(cause, SyntheticOriginKind::ImplicitReceiver);
                let ty = ResolvedTy::new(Ty::obj_name(singleton.classifier)).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })?;
                let value = self.body.add_expr(FirExpr {
                    origin,
                    ty,
                    kind: FirExprKind::SingletonValue {
                        classifier: singleton.classifier,
                    },
                });
                Ok((
                    FirCallableReferenceBinding::Bound,
                    Some(FirReceiver {
                        value,
                        conversion: None,
                    }),
                ))
            }
        }
    }

    fn reference_to_declaration(
        &self,
        expression: ExprId,
        declaration: DeclarationId,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let callable = self
            .index
            .callable_for_declaration(declaration)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        Ok(FirExprKind::CallableReference {
            target: callable.id.into(),
            function_type: self.reference_function_type(expression)?,
            reflective: self.reference_is_reflective(expression),
            binding,
            dispatch_receiver,
            extension_receiver,
            substitutions: self.call_substitutions(expression, declaration)?,
            adaptation: adaptation.map(Box::new),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn reference_to_external(
        &self,
        expression: ExprId,
        declaration: Option<ExternalCallableId>,
        default_provider: Option<ExternalCallableId>,
        compiler_intrinsic: Option<crate::libraries::CompilerIntrinsic>,
        receiver: Option<Ty>,
        extension_receiver_target: bool,
        parameters: &[Ty],
        result: Ty,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let declaration = declaration
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        let parameters = parameters
            .iter()
            .copied()
            .map(resolved)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(crate::libraries::CompilerIntrinsic::ArrayFactory(operation)) =
            compiler_intrinsic
        {
            if receiver.is_some()
                || extension_receiver_target
                || binding != FirCallableReferenceBinding::Static
                || dispatch_receiver.is_some()
                || extension_receiver.is_some()
            {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            }
            let array_type = resolved(result)?;
            let element_type = result
                .array_elem()
                .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
            return Ok(FirExprKind::CallableReference {
                target: FirCallableReferenceTarget::ArrayFactory {
                    operation,
                    array_type,
                    element_type: resolved(element_type)?,
                    parameters: parameters.into_boxed_slice(),
                },
                function_type: self.reference_function_type(expression)?,
                reflective: self.reference_is_reflective(expression),
                binding,
                dispatch_receiver: None,
                extension_receiver: None,
                substitutions: Box::new([]),
                adaptation: adaptation.map(Box::new),
            });
        }
        let substitutions = self
            .info
            .resolved_call_type_args
            .get(&expression)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ordinal, value)| {
                let ordinal = u32::try_from(ordinal)
                    .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
                let value = value.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                Ok(FirTypeSubstitution {
                    parameter: FirTypeParameterRef::External {
                        callable: declaration,
                        ordinal,
                    },
                    value: resolved(value)?,
                    additional_bounds: Box::new([]),
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        crate::trace_compiler!(
            "fir",
            "checked external callable reference expression={expression:?} target={declaration:?} receiver={receiver:?} extension={extension_receiver_target} parameters={parameters:?} result={result:?} binding={binding:?} adaptation={adaptation:?}",
        );
        Ok(FirExprKind::CallableReference {
            target: FirCallableReferenceTarget::External {
                declaration,
                default_provider,
                receiver: receiver.map(resolved).transpose()?,
                extension_receiver: extension_receiver_target,
                parameters: parameters.into_boxed_slice(),
                result: resolved(result)?,
            },
            function_type: self.reference_function_type(expression)?,
            reflective: self.reference_is_reflective(expression),
            binding,
            dispatch_receiver,
            extension_receiver,
            substitutions: substitutions.into_boxed_slice(),
            adaptation: adaptation.map(Box::new),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn property_reference_to_declaration(
        &self,
        expression: ExprId,
        declaration: DeclarationId,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        mutable: bool,
        receiver_type: Option<Ty>,
        extension_receiver_target: bool,
        property_type: Ty,
        adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        if self
            .index
            .declaration_header(declaration)
            .is_some_and(|header| {
                header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    && header
                        .flags
                        .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
            })
        {
            return Err(self.failure(
                self.file
                    .exact_member_name_spans
                    .get(&expression.0)
                    .copied()
                    .or(span),
                BodyCheckFailureKind::LocalVariableCallableReference,
            ));
        }
        let target = self
            .index
            .property_for_declaration(declaration)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        Ok(FirExprKind::PropertyReference {
            target: FirPropertyReferenceTarget::SpecializedModule {
                property: target,
                receiver: receiver_type.map(resolved).transpose()?,
                extension_receiver: extension_receiver_target,
                property_type: resolved(property_type)?,
            },
            function_type: self.reference_function_type(expression)?,
            reflective: self.reference_is_reflective(expression),
            binding,
            dispatch_receiver,
            extension_receiver,
            mutable,
            substitutions: self.call_substitutions(expression, declaration)?,
            adaptation: adaptation.map(Box::new),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn property_reference_to_external(
        &self,
        expression: ExprId,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        name: &str,
        receiver: Option<Ty>,
        extension_receiver_target: bool,
        getter: &crate::libraries::LibraryCallable,
        setter: Option<&crate::libraries::LibraryCallable>,
        property_type: Ty,
        adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let declaration = getter
            .external_identity
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let getter = self.external_property_reference_accessor(
            expression,
            receiver,
            extension_receiver_target,
            getter,
        )?;
        let setter = setter
            .map(|setter| {
                self.external_property_reference_accessor(
                    expression,
                    receiver,
                    extension_receiver_target,
                    setter,
                )
            })
            .transpose()?;
        let property_type = ResolvedTy::new(property_type)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let reflection_owner = receiver
            .map(ResolvedTy::new)
            .transpose()
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        let mutable = setter.is_some();
        Ok(FirExprKind::PropertyReference {
            target: FirPropertyReferenceTarget::External {
                name: name.into(),
                reflection_owner,
                getter: Box::new(getter),
                setter: setter.map(Box::new),
                extension_receiver: extension_receiver_target,
                property_type,
            },
            function_type: self.reference_function_type(expression)?,
            reflective: self.reference_is_reflective(expression),
            binding,
            dispatch_receiver,
            extension_receiver,
            mutable,
            substitutions: self.external_reference_substitutions(expression, declaration)?,
            adaptation: adaptation.map(Box::new),
        })
    }

    fn classifier_property_reference(
        &self,
        expression: ExprId,
        owner: crate::types::TypeName,
        property: FirClassifierProperty,
        property_type: Ty,
        adaptation: Option<FirReferenceAdaptation>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        Ok(FirExprKind::PropertyReference {
            target: FirPropertyReferenceTarget::Classifier {
                owner,
                property,
                property_type: resolved(property_type)?,
            },
            function_type: self.reference_function_type(expression)?,
            reflective: self.reference_is_reflective(expression),
            binding: FirCallableReferenceBinding::Static,
            dispatch_receiver: None,
            extension_receiver: None,
            mutable: false,
            substitutions: Box::new([]),
            adaptation: adaptation.map(Box::new),
        })
    }

    fn external_property_reference_accessor(
        &self,
        expression: ExprId,
        receiver: Option<Ty>,
        extension_receiver_target: bool,
        callable: &crate::libraries::LibraryCallable,
    ) -> Result<FirPropertyTarget, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let property = callable
            .external_property_identity
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let parameters = if extension_receiver_target {
            callable.params.get(1..).ok_or_else(|| {
                self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget)
            })?
        } else {
            callable.params.as_slice()
        };
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        Ok(FirPropertyTarget::External {
            property,
            receiver: receiver.map(resolved).transpose()?,
            parameters: parameters
                .iter()
                .copied()
                .map(resolved)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
            result: resolved(callable.ret)?,
            extension_receiver_parameter: None,
            dispatch: crate::fir::FirPropertyDispatch::Ordinary,
        })
    }

    fn external_reference_substitutions(
        &self,
        expression: ExprId,
        declaration: ExternalCallableId,
    ) -> Result<Box<[FirTypeSubstitution]>, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        self.info
            .resolved_call_type_args
            .get(&expression)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(ordinal, value)| {
                let ordinal = u32::try_from(ordinal)
                    .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
                let value = value.ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                Ok(FirTypeSubstitution {
                    parameter: FirTypeParameterRef::External {
                        callable: declaration,
                        ordinal,
                    },
                    value: resolved(value)?,
                    additional_bounds: Box::new([]),
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()
            .map(Vec::into_boxed_slice)
    }

    fn reference_adaptation(
        &self,
        expression: ExprId,
        arguments: Vec<AdaptedRefArgument>,
        parameter_types: &[Ty],
        result_type: Ty,
        suspend_conversion: bool,
    ) -> Result<FirReferenceAdaptation, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let arguments = arguments
            .into_iter()
            .map(|argument| match argument {
                AdaptedRefArgument::Value(value) => u32::try_from(value)
                    .map(FirAdaptedReferenceArgument::Value)
                    .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)),
                AdaptedRefArgument::Default => Ok(FirAdaptedReferenceArgument::Default),
                AdaptedRefArgument::Vararg {
                    values,
                    whole_array,
                } => values
                    .into_iter()
                    .map(|value| {
                        u32::try_from(value).map_err(|_| {
                            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|values| FirAdaptedReferenceArgument::Vararg {
                        values: values.into_boxed_slice(),
                        whole_array,
                    }),
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let parameter_types = parameter_types
            .iter()
            .copied()
            .map(|ty| {
                ResolvedTy::new(ty).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let result_type = ResolvedTy::new(result_type)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        Ok(FirReferenceAdaptation {
            arguments,
            parameter_types,
            result_type,
            suspend_conversion,
        })
    }
}
