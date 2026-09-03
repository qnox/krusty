use crate::fir::{
    CallableId, FirCall, FirCallArgument, FirCallTarget, FirConstructorCall, FirConstructorTarget,
    FirPropertyReferenceTarget, FirPropertyTarget, FirReceiver, FirTypeParameterRef,
    FirTypeSubstitution,
};
use crate::ir::{
    ExprId, IrCheckedArgument, IrCheckedConstructorTarget, IrCheckedOperation,
    IrCheckedSubstitution, IrExpr, IrFunction,
};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    pub(super) fn shared_cell_new(
        &mut self,
        element: crate::fir::ResolvedTy,
        initial: Option<ExprId>,
    ) -> ExprId {
        let initial =
            initial.unwrap_or_else(|| self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null)));
        self.ir.add_expr(IrExpr::RefNew {
            elem: element.get(),
            init: initial,
        })
    }

    pub(super) fn shared_cell_read(
        &mut self,
        slot: u32,
        element: crate::fir::ResolvedTy,
    ) -> ExprId {
        let holder = self.ir.add_expr(IrExpr::GetValue(slot));
        self.ir.add_expr(IrExpr::RefGet {
            elem: element.get(),
            holder,
        })
    }

    pub(super) fn shared_cell_write(
        &mut self,
        slot: u32,
        element: crate::fir::ResolvedTy,
        value: ExprId,
    ) -> ExprId {
        let holder = self.ir.add_expr(IrExpr::GetValue(slot));
        self.ir.add_expr(IrExpr::RefSet {
            elem: element.get(),
            holder,
            value,
        })
    }

    pub(super) fn checked_singleton(&mut self, classifier: crate::types::TypeName) -> ExprId {
        self.ir.add_expr(IrExpr::SingletonValue { classifier })
    }

    pub(super) fn checked_class_literal(
        &mut self,
        classifier: Option<crate::fir::ResolvedTy>,
        value: Option<crate::fir::FirExprId>,
    ) -> Result<ExprId, FirLoweringFailure> {
        let value = value.map(|value| self.expression(value)).transpose()?;
        Ok(self.ir.add_expr(IrExpr::KClassLiteral {
            classifier: classifier.map(crate::fir::ResolvedTy::get),
            value,
        }))
    }

    /// Read one parameter of a structural callable-reference adapter in the representation expected
    /// by the checker-selected declaration parameter. Kotlin's nullable bottom type (`Nothing?`)
    /// is assignable to every nullable reference, but a backend may represent the adapter slot as
    /// `Void`; retaining the checked coercion gives it the target reference type without repeating
    /// subtype analysis during emission.
    fn adapted_reference_argument_read(
        &mut self,
        slot: u32,
        source: crate::types::Ty,
        target: crate::types::Ty,
    ) -> ExprId {
        let read = self.ir.add_expr(IrExpr::GetValue(slot));
        if source != target
            && source.non_null() == crate::types::Ty::Nothing
            && target.is_reference()
        {
            self.ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: read,
                type_operand: target,
            })
        } else {
            read
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn checked_callable_reference(
        &mut self,
        target: crate::fir::FirCallableReferenceTarget,
        binding: crate::fir::FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        substitutions: &[FirTypeSubstitution],
        adaptation: Option<&crate::fir::FirReferenceAdaptation>,
        reference_ty: crate::types::Ty,
        reflective: bool,
    ) -> Result<ExprId, FirLoweringFailure> {
        if let crate::fir::FirCallableReferenceTarget::Constructor {
            target,
            classifier,
            outer,
            parameters,
            result,
        } = &target
        {
            return self.materialize_constructor_reference(
                target,
                *classifier,
                *outer,
                parameters,
                *result,
                binding,
                dispatch_receiver,
                adaptation,
                reference_ty,
                reflective,
            );
        }
        if let crate::fir::FirCallableReferenceTarget::Classifier {
            classifier,
            operation,
            parameters,
            result,
        } = &target
        {
            if dispatch_receiver.is_some() || extension_receiver.is_some() {
                return Err(FirLoweringFailure::UnsupportedClassifierCallableReference(
                    *classifier,
                ));
            }
            return self.checked_classifier_callable_reference(
                *classifier,
                operation.clone(),
                parameters,
                *result,
                binding,
                adaptation,
                reference_ty,
            );
        }
        let crate::fir::FirCallableReferenceTarget::Module(target) = target else {
            if reflective {
                let dispatch_receiver = self.receiver(dispatch_receiver)?;
                let extension_receiver = self.receiver(extension_receiver)?;
                return Ok(self.ir.add_expr(IrExpr::Checked(
                    IrCheckedOperation::CallableReference {
                        target,
                        binding,
                        dispatch_receiver,
                        extension_receiver,
                        function_type: reference_ty,
                        substitutions: lower_substitutions(substitutions),
                        adaptation: adaptation.cloned().map(Box::new),
                    },
                )));
            }
            return self.checked_external_callable_reference(
                target,
                binding,
                dispatch_receiver,
                extension_receiver,
                adaptation,
                reference_ty,
            );
        };
        let callable = self
            .index
            .callable(target)
            .ok_or(FirLoweringFailure::MissingCallable(target))?;
        let dispatch_receiver = self.receiver(dispatch_receiver)?;
        let extension_receiver = self.receiver(extension_receiver)?;
        if let Some(reference) = self.materialize_callable_reference(
            callable,
            binding,
            dispatch_receiver,
            extension_receiver,
            substitutions,
            adaptation,
            reference_ty,
        )? {
            return Ok(reference);
        }
        Err(FirLoweringFailure::UnsupportedCallableReference(target))
    }

    fn materialize_callable_reference(
        &mut self,
        callable: crate::fir::ResolvedCallableHeader,
        binding: crate::fir::FirCallableReferenceBinding,
        dispatch_capture: Option<ExprId>,
        extension_capture: Option<ExprId>,
        substitutions: &[FirTypeSubstitution],
        adaptation: Option<&crate::fir::FirReferenceAdaptation>,
        reference_ty: crate::types::Ty,
    ) -> Result<Option<ExprId>, FirLoweringFailure> {
        let crate::types::Ty::Fun(reference) = reference_ty.non_null() else {
            return Ok(None);
        };
        let Some(arity) = u8::try_from(reference.params.len()).ok() else {
            return Ok(None);
        };
        let signature = self
            .index
            .signature(callable.declaration)
            .ok_or(FirLoweringFailure::MissingCallable(callable.id))?;
        let enclosing = self.index.enclosing_classifier(callable.declaration);
        let substitution_bindings = self.module_substitution_bindings(substitutions);
        let specialize = |ty| crate::types::ty_subst_keep_unbound(ty, &substitution_bindings);
        let signature_parameters = signature
            .parameters
            .iter()
            .map(|parameter| specialize(parameter.get()))
            .collect::<Vec<_>>();
        let signature_result = specialize(signature.result.get());
        crate::trace_compiler!(
            "lower",
            "materialize callable reference target={:?} binding={binding:?} reference={reference:?} signature={signature:?} enclosing={:?} dispatch={} extension={} adaptation={adaptation:?} substitutions={substitutions:?}",
            callable.id,
            enclosing.map(|owner| owner.classifier),
            dispatch_capture.is_some(),
            extension_capture.is_some(),
        );

        if adaptation.is_none() {
            if let Some(reference) = self.materialize_structural_module_function_reference(
                callable,
                binding,
                dispatch_capture,
                extension_capture,
                reference,
                signature,
                enclosing,
            )? {
                return Ok(Some(reference));
            }
        }

        let mut captures = Vec::new();
        let mut capture_types = Vec::new();
        let dispatch_capture_slot = dispatch_capture.map(|value| {
            let slot = captures.len() as u32;
            captures.push(value);
            capture_types.push(crate::types::Ty::obj_name(
                enclosing
                    .expect("a checked dispatch receiver has a classifier")
                    .classifier,
            ));
            slot
        });
        let extension_capture_slot = extension_capture.map(|value| {
            let slot = captures.len() as u32;
            captures.push(value);
            capture_types.push(specialize(
                callable
                    .shape
                    .extension_receiver
                    .expect("a checked extension receiver has a type")
                    .get(),
            ));
            slot
        });

        let own_start = captures.len() as u32;
        let mut own_parameters = reference.params.clone();
        let mut own_parameter_slots = (0..reference.params.len() as u32).collect::<Vec<_>>();
        let mut dispatch_receiver =
            dispatch_capture_slot.map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)));
        let mut extension_receiver =
            extension_capture_slot.map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)));
        if binding == crate::fir::FirCallableReferenceBinding::Unbound {
            if dispatch_receiver.is_none() && enclosing.is_some() {
                let Some(receiver_ty) = own_parameters.first().copied() else {
                    return Ok(None);
                };
                let expected = crate::types::Ty::obj_name(
                    enclosing
                        .expect("an unbound member has an enclosing classifier")
                        .classifier,
                );
                if receiver_ty != expected {
                    return Ok(None);
                }
                dispatch_receiver = Some(self.ir.add_expr(IrExpr::GetValue(own_start)));
                own_parameters.remove(0);
                own_parameter_slots.remove(0);
            } else if extension_receiver.is_none() {
                if let Some(expected) = callable.shape.extension_receiver {
                    let position = callable.shape.context_parameter_count as usize;
                    if own_parameters.get(position).copied() != Some(specialize(expected.get())) {
                        return Ok(None);
                    }
                    extension_receiver = Some(
                        self.ir
                            .add_expr(IrExpr::GetValue(own_start + position as u32)),
                    );
                    own_parameters.remove(position);
                    own_parameter_slots.remove(position);
                }
            }
        }
        if adaptation.is_none() && own_parameters.len() != signature_parameters.len() {
            return Ok(None);
        }

        let arguments = if let Some(adaptation) = adaptation {
            if adaptation.arguments.len() != signature_parameters.len() {
                return Ok(None);
            }
            let mut arguments = Vec::with_capacity(adaptation.arguments.len());
            for (parameter, argument) in adaptation.arguments.iter().enumerate() {
                let parameter = parameter as u32;
                arguments.push(match argument {
                    crate::fir::FirAdaptedReferenceArgument::Value(source) => {
                        let Some(&source_ty) = reference.params.get(*source as usize) else {
                            return Ok(None);
                        };
                        IrCheckedArgument::Expression {
                            parameter,
                            value: self.adapted_reference_argument_read(
                                own_start + *source,
                                source_ty,
                                signature_parameters[parameter as usize],
                            ),
                        }
                    }
                    crate::fir::FirAdaptedReferenceArgument::Default => {
                        IrCheckedArgument::Default { parameter }
                    }
                    crate::fir::FirAdaptedReferenceArgument::Vararg {
                        values,
                        whole_array,
                    } => {
                        let array_type = signature_parameters[parameter as usize];
                        let mut elements = Vec::with_capacity(values.len());
                        for source in values.iter().copied() {
                            let Some(_) = reference.params.get(source as usize) else {
                                return Ok(None);
                            };
                            elements.push((
                                self.ir.add_expr(IrExpr::GetValue(own_start + source)),
                                *whole_array,
                            ));
                        }
                        IrCheckedArgument::Vararg {
                            parameter,
                            array_type,
                            elements,
                        }
                    }
                });
            }
            arguments
        } else {
            own_parameters
                .iter()
                .zip(own_parameter_slots)
                .enumerate()
                .map(|(parameter, (_, source))| IrCheckedArgument::Expression {
                    parameter: parameter as u32,
                    value: self.ir.add_expr(IrExpr::GetValue(own_start + source)),
                })
                .collect::<Vec<_>>()
        };
        // The adapter body is a separate JVM method. Its value slots start with the captured
        // receivers followed by the function-type parameters; call normalization may allocate
        // temporaries and must place them above that wrapper-local prefix, not above the enclosing
        // source body's locals.
        let enclosing_temporary = self.next_temporary;
        self.next_temporary = u32::try_from(captures.len() + reference.params.len())
            .expect("too many callable-reference adapter parameters");
        let call = self.same_file_call(
            callable.id,
            dispatch_receiver,
            extension_receiver,
            &arguments,
            &signature_parameters,
            substitutions,
        );
        self.next_temporary = enclosing_temporary;
        let Some(call) = call else {
            return Ok(None);
        };
        let body = self.callable_reference_adapter_body(call, signature_result, reference.ret);
        let mut parameters = capture_types;
        parameters.extend(reference.params.iter().copied());
        let adapter_name = format!(
            "$fir_callable_ref_{}_{}",
            self.body.owner().raw(),
            self.ir.functions.len()
        );
        let function = self.ir.add_fun(IrFunction {
            name: adapter_name.clone(),
            params: parameters.clone(),
            ret: crate::types::stored_value_ty(reference.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        if reference.suspend {
            self.ir.suspend_funs.push(function);
        }
        self.ir.private_methods.insert(function);
        self.ir
            .lambda_own_params_from
            .insert(function, captures.len() as u32);
        if let Some(owner) = self
            .index
            .enclosing_classifier(crate::fir::DeclarationId::from_raw(self.body.owner().raw()))
        {
            if let Some(class) = self
                .ir
                .checked_classifier_classes
                .get(&owner.declaration)
                .copied()
            {
                self.ir.classes[class as usize].methods.push(function);
            }
        }
        if let Some(adaptation) = adaptation {
            let capture = match captures.as_slice() {
                [] => None,
                [capture] => Some(*capture),
                _ => {
                    return Err(FirLoweringFailure::UnsupportedCallableReference(
                        callable.id,
                    ));
                }
            };
            return self
                .materialize_structural_adapted_module_reference(
                    callable,
                    reference,
                    &signature_parameters,
                    signature_result,
                    adapter_name,
                    function,
                    parameters,
                    capture,
                    adaptation,
                    enclosing,
                )
                .map(Some);
        }
        Ok(Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: function,
            arity,
            captures,
            sam: None,
            inline_body: None,
        })))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn checked_property_reference(
        &mut self,
        target: &FirPropertyReferenceTarget,
        binding: crate::fir::FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        mutable: bool,
        substitutions: &[FirTypeSubstitution],
        adaptation: Option<&crate::fir::FirReferenceAdaptation>,
        reference_ty: crate::types::Ty,
        reflective: bool,
    ) -> Result<ExprId, FirLoweringFailure> {
        match target {
            FirPropertyReferenceTarget::Module(target) => {
                self.index
                    .property(*target)
                    .ok_or(FirLoweringFailure::MissingProperty(*target))?;
            }
            FirPropertyReferenceTarget::SpecializedModule { property, .. } => {
                self.index
                    .property(*property)
                    .ok_or(FirLoweringFailure::MissingProperty(*property))?;
            }
            FirPropertyReferenceTarget::Classifier { .. } => {}
            FirPropertyReferenceTarget::External { getter, setter, .. } => {
                if !matches!(getter.as_ref(), FirPropertyTarget::External { .. })
                    || setter
                        .as_deref()
                        .is_some_and(|setter| !matches!(setter, FirPropertyTarget::External { .. }))
                {
                    return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
                }
            }
        }
        let dispatch_receiver = self.receiver(dispatch_receiver)?;
        let extension_receiver = self.receiver(extension_receiver)?;
        if !reflective {
            if let Some(reference) = self.materialize_property_function_reference(
                target,
                binding,
                dispatch_receiver,
                extension_receiver,
                adaptation,
                reference_ty,
            )? {
                return Ok(reference);
            }
        }
        Ok(self
            .ir
            .add_expr(IrExpr::Checked(IrCheckedOperation::PropertyReference {
                target: target.clone(),
                binding,
                dispatch_receiver,
                extension_receiver,
                mutable,
                substitutions: lower_substitutions(substitutions),
                adaptation: adaptation.cloned().map(Box::new),
            })))
    }

    pub(super) fn checked_call(&mut self, call: &FirCall) -> Result<ExprId, FirLoweringFailure> {
        let dispatch_receiver = self.receiver(call.dispatch_receiver)?;
        let extension_receiver = self.receiver(call.extension_receiver)?;
        match &call.target {
            FirCallTarget::Module(target) => {
                self.index
                    .callable(*target)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?;
                let parameter_types = call
                    .parameter_types
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let arguments = self.arguments(*target, &call.arguments, &parameter_types)?;
                if let Some(call) = self.same_file_call(
                    *target,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                    &parameter_types,
                    &call.substitutions,
                ) {
                    return Ok(call);
                }
                let operation = IrCheckedOperation::Call {
                    target: *target,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                    substitutions: lower_substitutions(&call.substitutions),
                };
                Ok(self.ir.add_expr(IrExpr::Checked(operation)))
            }
            FirCallTarget::External {
                declaration,
                receiver,
                declared_receiver,
                parameters,
                result,
                declared_result,
                suspend,
                can_inline,
                inline_plan,
                extension_receiver_parameter,
            } => {
                let arguments = self.external_arguments(&call.arguments, parameters)?;
                self.external_call(
                    *declaration,
                    *receiver,
                    *declared_receiver,
                    parameters,
                    *result,
                    *declared_result,
                    *suspend,
                    *can_inline,
                    inline_plan.as_deref(),
                    &call.substitutions,
                    *extension_receiver_parameter,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                )
                .ok_or(FirLoweringFailure::UnsupportedExternalCall(*declaration))
            }
            FirCallTarget::Super {
                owner,
                name,
                parameters,
                result,
                interface,
                descriptor,
                physical_result,
                source,
                source_member,
            } => {
                let _ = result;
                let checked = self.external_arguments(&call.arguments, parameters)?;
                let (statements, receiver, arguments, defaults) = self
                    .selected_semantic_operands(
                        None,
                        &parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>(),
                        None,
                        None,
                        &checked,
                        true,
                        false,
                        None,
                    )
                    .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)?;
                debug_assert!(receiver.is_none());
                let dispatch_receiver =
                    dispatch_receiver.ok_or(FirLoweringFailure::UnsupportedCallableReference(
                        crate::fir::CallableId::from_raw(0),
                    ))?;
                // The semantic super target stays explicit through common lowering. A provider may
                // already have fixed its physical descriptor; source declarations leave it empty.
                let callee = crate::ir::Callee::Super {
                    owner: *owner,
                    name: name.clone(),
                    params: parameters.iter().map(|parameter| parameter.get()).collect(),
                    ret: physical_result.get(),
                    interface: *interface,
                    descriptor: descriptor.clone(),
                    source: *source,
                    defaults,
                    source_member: source_member.clone(),
                };
                let call = self.ir.add_expr(IrExpr::Call {
                    callee,
                    dispatch_receiver: Some(dispatch_receiver),
                    args: arguments,
                });
                Ok(self.wrap_call_statements(statements, call))
            }
            FirCallTarget::Intrinsic {
                operation,
                receiver,
                parameters,
                result,
            } => {
                if matches!(
                    operation,
                    crate::fir::FirIntrinsic::SuspendCoroutine
                        | crate::fir::FirIntrinsic::SuspendCoroutineUninterceptedOrReturn
                ) {
                    return self
                        .suspend_coroutine_primitive(
                            operation,
                            dispatch_receiver,
                            extension_receiver,
                            &call.arguments,
                            *result,
                        )
                        .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall);
                }
                let arguments = self.external_arguments(&call.arguments, parameters)?;
                self.intrinsic_call(
                    lower_fir_intrinsic(operation),
                    *receiver,
                    parameters,
                    *result,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                )
                .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)
            }
            FirCallTarget::Classifier {
                classifier,
                operation,
                parameters,
                result,
            } => {
                if dispatch_receiver.is_some() || extension_receiver.is_some() {
                    return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                }
                let arguments = self.external_arguments(&call.arguments, parameters)?;
                let arguments = arguments
                    .into_iter()
                    .map(|argument| match argument {
                        crate::ir::IrCheckedArgument::Expression { value, .. } => Ok(value),
                        crate::ir::IrCheckedArgument::Default { .. }
                        | crate::ir::IrCheckedArgument::Vararg { .. } => {
                            Err(FirLoweringFailure::UnsupportedIntrinsicCall)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                match (operation, arguments.as_slice()) {
                    (crate::fir::FirClassifierCallable::EnumValues, []) => {
                        Ok(self.ir.add_expr(IrExpr::EnumValues {
                            classifier: *classifier,
                        }))
                    }
                    (crate::fir::FirClassifierCallable::EnumValueOf, [argument]) => {
                        Ok(self.ir.add_expr(IrExpr::EnumValueOf {
                            classifier: *classifier,
                            arg: *argument,
                        }))
                    }
                    (crate::fir::FirClassifierCallable::ArrayConstructor { element }, [size]) => {
                        self.array_construction_from_values(*result, *element, *size, None)
                    }
                    (
                        crate::fir::FirClassifierCallable::ArrayConstructor { element },
                        [size, initializer],
                    ) => self.array_construction_from_values(
                        *result,
                        *element,
                        *size,
                        Some((*initializer, parameters[1].get())),
                    ),
                    _ => Err(FirLoweringFailure::UnsupportedIntrinsicCall),
                }
            }
        }
    }

    pub(super) fn checked_constructor_call(
        &mut self,
        call: &FirConstructorCall,
    ) -> Result<ExprId, FirLoweringFailure> {
        match &call.target {
            FirConstructorTarget::Module(target) => {
                let callable = self
                    .index
                    .callable(*target)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?;
                let classifier = self
                    .index
                    .enclosing_classifier(callable.declaration)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?;
                let parameter_types = call
                    .parameter_types
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let declaration_parameter_types = self
                    .index
                    .signature(callable.declaration)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let arguments = self.arguments(*target, &call.arguments, &parameter_types)?;
                let outer_receiver = self.receiver(call.outer_receiver)?;
                let anchor = self
                    .index
                    .declaration_anchor(callable.declaration)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?;
                let primary_in_current_file = anchor.sibling == 0
                    && self.ir.class_id_by_name(classifier.classifier).is_some();
                self.module_constructor_call(
                    *target,
                    classifier.classifier,
                    &parameter_types,
                    &declaration_parameter_types,
                    primary_in_current_file,
                    outer_receiver,
                    &arguments,
                )
                .ok_or(FirLoweringFailure::UnsupportedModuleConstructor(*target))
            }
            FirConstructorTarget::External {
                declaration,
                classifier,
                parameters,
            } => {
                let arguments = self.external_arguments(&call.arguments, parameters)?;
                let outer_receiver = self.receiver(call.outer_receiver)?;
                self.external_constructor_call(
                    *declaration,
                    *classifier,
                    parameters,
                    outer_receiver,
                    &arguments,
                )
                .ok_or(FirLoweringFailure::UnsupportedExternalConstructor(
                    *declaration,
                ))
            }
        }
    }

    pub(super) fn checked_constructor_delegation(
        &mut self,
        call: &FirConstructorCall,
    ) -> Result<ExprId, FirLoweringFailure> {
        let (target, arguments) = self.constructor_target_and_arguments(call)?;
        let outer_receiver = self.receiver(call.outer_receiver)?;
        Ok(self
            .ir
            .add_expr(IrExpr::Checked(IrCheckedOperation::ConstructorDelegation {
                target,
                outer_parameter: call.outer_parameter.map(crate::fir::ResolvedTy::get),
                outer_receiver,
                arguments,
                substitutions: lower_substitutions(&call.substitutions),
            })))
    }

    fn constructor_target_and_arguments(
        &mut self,
        call: &FirConstructorCall,
    ) -> Result<(IrCheckedConstructorTarget, Vec<IrCheckedArgument>), FirLoweringFailure> {
        match &call.target {
            FirConstructorTarget::Module(target) => {
                self.index
                    .callable(*target)
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?;
                Ok((
                    IrCheckedConstructorTarget::Module(*target),
                    self.arguments(
                        *target,
                        &call.arguments,
                        &call
                            .parameter_types
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect::<Vec<_>>(),
                    )?,
                ))
            }
            FirConstructorTarget::External {
                declaration,
                classifier,
                parameters,
            } => {
                let arguments = self.external_arguments(&call.arguments, parameters)?;
                Ok((
                    IrCheckedConstructorTarget::External {
                        declaration: *declaration,
                        classifier: *classifier,
                        parameters: parameters.iter().map(|parameter| parameter.get()).collect(),
                    },
                    arguments,
                ))
            }
        }
    }

    pub(super) fn checked_property_read(
        &mut self,
        target: &FirPropertyTarget,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        context_arguments: &[FirReceiver],
        substitutions: &[FirTypeSubstitution],
    ) -> Result<ExprId, FirLoweringFailure> {
        match target {
            FirPropertyTarget::Module(target) => {
                self.index
                    .property(*target)
                    .ok_or(FirLoweringFailure::MissingProperty(*target))?;
                let operation = IrCheckedOperation::PropertyRead {
                    target: *target,
                    dispatch_receiver: self.receiver(dispatch_receiver)?,
                    extension_receiver: self.receiver(extension_receiver)?,
                    context_arguments: context_arguments
                        .iter()
                        .copied()
                        .map(|receiver| {
                            self.expression_with_conversion(receiver.value, receiver.conversion)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    substitutions: lower_substitutions(substitutions),
                };
                Ok(self.ir.add_expr(IrExpr::Checked(operation)))
            }
            FirPropertyTarget::External {
                property,
                receiver,
                parameters,
                result,
                extension_receiver_parameter,
                dispatch,
            } => {
                let arguments = context_arguments
                    .iter()
                    .enumerate()
                    .map(|(parameter, receiver)| FirCallArgument::Expression {
                        parameter: parameter as u32,
                        value: receiver.value,
                        conversion: receiver.conversion,
                    })
                    .collect::<Vec<_>>();
                let arguments = self.external_arguments(&arguments, parameters)?;
                let dispatch_receiver = self.receiver(dispatch_receiver)?;
                let extension_receiver = self.receiver(extension_receiver)?;
                self.external_property_access(
                    *property,
                    dispatch.clone(),
                    *receiver,
                    parameters,
                    *result,
                    *extension_receiver_parameter,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                    false,
                )
                .ok_or(FirLoweringFailure::UnsupportedExternalProperty(*property))
            }
        }
    }

    pub(super) fn checked_property_write(
        &mut self,
        target: &FirPropertyTarget,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        context_arguments: &[FirReceiver],
        value: crate::fir::FirExprId,
        conversion: Option<crate::fir::FirConversion>,
        substitutions: &[FirTypeSubstitution],
    ) -> Result<ExprId, FirLoweringFailure> {
        match target {
            FirPropertyTarget::Module(target) => {
                self.index
                    .property(*target)
                    .ok_or(FirLoweringFailure::MissingProperty(*target))?;
                let operation = IrCheckedOperation::PropertyWrite {
                    target: *target,
                    dispatch_receiver: self.receiver(dispatch_receiver)?,
                    extension_receiver: self.receiver(extension_receiver)?,
                    context_arguments: context_arguments
                        .iter()
                        .copied()
                        .map(|receiver| {
                            self.expression_with_conversion(receiver.value, receiver.conversion)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    value: self.expression_with_conversion(value, conversion)?,
                    substitutions: lower_substitutions(substitutions),
                };
                Ok(self.ir.add_expr(IrExpr::Checked(operation)))
            }
            FirPropertyTarget::External {
                property,
                receiver,
                parameters,
                result,
                extension_receiver_parameter,
                dispatch,
            } => {
                let parameter = u32::try_from(
                    parameters
                        .len()
                        .checked_sub(1)
                        .ok_or(FirLoweringFailure::MissingExternalParameter { parameter: 0 })?,
                )
                .map_err(|_| FirLoweringFailure::MissingExternalParameter {
                    parameter: u32::MAX,
                })?;
                let arguments = context_arguments
                    .iter()
                    .enumerate()
                    .map(|(parameter, receiver)| FirCallArgument::Expression {
                        parameter: parameter as u32,
                        value: receiver.value,
                        conversion: receiver.conversion,
                    })
                    .chain(std::iter::once(FirCallArgument::Expression {
                        parameter,
                        value,
                        conversion,
                    }))
                    .collect::<Vec<_>>();
                let arguments = self.external_arguments(&arguments, parameters)?;
                let dispatch_receiver = self.receiver(dispatch_receiver)?;
                let extension_receiver = self.receiver(extension_receiver)?;
                self.external_property_access(
                    *property,
                    dispatch.clone(),
                    *receiver,
                    parameters,
                    *result,
                    *extension_receiver_parameter,
                    dispatch_receiver,
                    extension_receiver,
                    &arguments,
                    true,
                )
                .ok_or(FirLoweringFailure::UnsupportedExternalProperty(*property))
            }
        }
    }

    pub(super) fn receiver(
        &mut self,
        receiver: Option<FirReceiver>,
    ) -> Result<Option<ExprId>, FirLoweringFailure> {
        receiver
            .map(|receiver| self.expression_with_conversion(receiver.value, receiver.conversion))
            .transpose()
    }

    fn arguments(
        &mut self,
        target: CallableId,
        arguments: &[FirCallArgument],
        parameter_types: &[crate::types::Ty],
    ) -> Result<Vec<IrCheckedArgument>, FirLoweringFailure> {
        self.lower_arguments(arguments, |parameter| {
            parameter_types
                .get(parameter as usize)
                .copied()
                .ok_or(FirLoweringFailure::MissingParameter { target, parameter })
        })
    }

    fn module_substitution_bindings(
        &self,
        substitutions: &[FirTypeSubstitution],
    ) -> std::collections::HashMap<String, crate::types::Ty> {
        substitutions
            .iter()
            .filter_map(|substitution| match substitution.parameter {
                FirTypeParameterRef::Module(parameter) => self
                    .index
                    .type_parameter_semantic_name(parameter)
                    .map(|name| (name.to_owned(), substitution.value.get())),
                FirTypeParameterRef::External { .. } => None,
            })
            .collect()
    }

    fn external_arguments(
        &mut self,
        arguments: &[FirCallArgument],
        parameters: &[crate::fir::ResolvedTy],
    ) -> Result<Vec<IrCheckedArgument>, FirLoweringFailure> {
        self.lower_arguments(arguments, |parameter| {
            parameters
                .get(parameter as usize)
                .map(|parameter| parameter.get())
                .ok_or(FirLoweringFailure::MissingExternalParameter { parameter })
        })
    }

    pub(super) fn lower_arguments(
        &mut self,
        arguments: &[FirCallArgument],
        parameter_type: impl Fn(u32) -> Result<crate::types::Ty, FirLoweringFailure>,
    ) -> Result<Vec<IrCheckedArgument>, FirLoweringFailure> {
        arguments
            .iter()
            .map(|argument| match argument {
                FirCallArgument::Expression {
                    parameter,
                    value,
                    conversion,
                } => Ok(IrCheckedArgument::Expression {
                    parameter: *parameter,
                    value: self.expression_with_conversion(*value, *conversion)?,
                }),
                FirCallArgument::Default { parameter, .. } => Ok(IrCheckedArgument::Default {
                    parameter: *parameter,
                }),
                FirCallArgument::Vararg {
                    parameter,
                    elements,
                    ..
                } => Ok(IrCheckedArgument::Vararg {
                    parameter: *parameter,
                    array_type: parameter_type(*parameter)?,
                    elements: elements
                        .iter()
                        .map(|element| {
                            Ok((
                                self.expression_with_conversion(element.value, element.conversion)?,
                                element.spread,
                            ))
                        })
                        .collect::<Result<Vec<_>, FirLoweringFailure>>()?,
                }),
            })
            .collect()
    }
}

pub(super) fn lower_substitutions(values: &[FirTypeSubstitution]) -> Vec<IrCheckedSubstitution> {
    values
        .iter()
        .map(|substitution| IrCheckedSubstitution {
            parameter: substitution.parameter,
            value: substitution.value.get(),
            additional_bounds: substitution
                .additional_bounds
                .iter()
                .map(|bound| bound.get())
                .collect(),
        })
        .collect()
}

pub(super) fn lower_fir_intrinsic(operation: &crate::fir::FirIntrinsic) -> crate::ir::IrIntrinsic {
    match operation {
        crate::fir::FirIntrinsic::ArrayGet => crate::ir::IrIntrinsic::ArrayGet,
        crate::fir::FirIntrinsic::ArraySet => crate::ir::IrIntrinsic::ArraySet,
        crate::fir::FirIntrinsic::ArraySize => crate::ir::IrIntrinsic::ArraySize,
        crate::fir::FirIntrinsic::StringGet => crate::ir::IrIntrinsic::StringGet,
        crate::fir::FirIntrinsic::StringLength => crate::ir::IrIntrinsic::StringLength,
        crate::fir::FirIntrinsic::StringPlus => crate::ir::IrIntrinsic::StringPlus,
        crate::fir::FirIntrinsic::NullableAnyToString => {
            crate::ir::IrIntrinsic::NullableAnyToString
        }
        crate::fir::FirIntrinsic::PrimitiveCompare { operand } => {
            crate::ir::IrIntrinsic::PrimitiveCompare {
                operand: operand.get(),
            }
        }
        crate::fir::FirIntrinsic::CoroutineContext => crate::ir::IrIntrinsic::CoroutineContext,
        crate::fir::FirIntrinsic::SuspendCoroutine => {
            unreachable!(
                "safe suspend coroutine blocks are structurally lowered before this mapping"
            )
        }
        crate::fir::FirIntrinsic::SuspendCoroutineUninterceptedOrReturn => {
            unreachable!("suspend coroutine blocks are structurally lowered before this mapping")
        }
        crate::fir::FirIntrinsic::UnsignedToString { source } => {
            crate::ir::IrIntrinsic::UnsignedToString {
                source: source.get(),
            }
        }
        crate::fir::FirIntrinsic::PrimitiveArrayNew { element } => {
            crate::ir::IrIntrinsic::PrimitiveArrayNew {
                element: element.get(),
            }
        }
    }
}
