//! Materialization of language-defined classifier callable references.

use crate::fir::{
    FirAdaptedReferenceArgument, FirCallableReferenceBinding, FirClassifierCallable,
    FirReferenceAdaptation, ResolvedTy,
};
use crate::ir::{ExprId, IrExpr, IrFunction};
use crate::types::{Ty, TypeName};

use super::source_calls::ModuleConstructorRequest;
use super::{BodyLowering, FirLoweringFailure};

fn unsupported_constructor_reference(
    target: &crate::fir::FirConstructorTarget,
) -> FirLoweringFailure {
    match target {
        crate::fir::FirConstructorTarget::Module(target) => {
            FirLoweringFailure::UnsupportedCallableReference(*target)
        }
        crate::fir::FirConstructorTarget::External { declaration, .. } => {
            FirLoweringFailure::UnsupportedExternalCallableReference(*declaration)
        }
    }
}

impl BodyLowering<'_> {
    /// Materialize `::A` as a structural constructor reference whose invocation delegates to one
    /// checked static adapter. The runtime reflection signature remains the constructor's
    /// `(<parameters>)V`, independent of that adapter's value-returning ABI.
    pub(super) fn materialize_constructor_reference(
        &mut self,
        target: &crate::fir::FirConstructorTarget,
        classifier: TypeName,
        outer: Option<ResolvedTy>,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<crate::fir::FirReceiver>,
        adaptation: Option<&FirReferenceAdaptation>,
        reference_ty: Ty,
        reflective: bool,
    ) -> Result<ExprId, FirLoweringFailure> {
        let failed = || unsupported_constructor_reference(target);
        let Ty::Fun(reference) = reference_ty.non_null() else {
            return Err(failed());
        };
        let arity = u8::try_from(reference.params.len()).map_err(|_| failed())?;

        // Every constructor reference invokes through a checked static adapter. A plain reference's
        // adapter signature is already the reflected constructor signature; an unadapted inner
        // reference additionally carries its checked outer parameter/capture. Adapted reflective
        // references need a distinct reflected-parameter signature from their adapter ABI, which the
        // current common-IR carrier does not yet represent.
        let simple = outer.is_none() && dispatch_receiver.is_none() && adaptation.is_none();
        if simple {
            if reference.params.len() != parameters.len() {
                return Err(failed());
            }
            let arguments = (0..parameters.len())
                .map(|parameter| crate::ir::IrCheckedArgument::Expression {
                    parameter: parameter as u32,
                    value: self.ir.add_expr(IrExpr::GetValue(parameter as u32)),
                })
                .collect::<Vec<_>>();
            let construction = self.constructor_reference_call(
                target, classifier, parameters, None, None, &arguments,
            )?;
            let body =
                self.callable_reference_adapter_body(construction, result.get(), reference.ret);
            let adapter_name = format!(
                "$fir_ctor_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            );
            let function = self.ir.add_fun(IrFunction {
                name: adapter_name.clone(),
                params: reference.params.to_vec(),
                ret: crate::types::stored_value_ty(reference.ret),
                body: Some(body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
            self.ir.private_methods.insert(function);

            return Ok(self.structural_constructor_reference(
                classifier,
                function,
                reference_ty,
                reference.params.clone(),
                None,
            ));
        }
        if reflective && adaptation.is_some() {
            return Err(failed());
        }

        let dispatch_receiver = self.receiver(dispatch_receiver)?;
        let mut captures = Vec::new();
        let mut wrapper_parameters = Vec::new();
        let (outer_receiver, own_start, identity_source_start) = match (outer, binding) {
            (Some(outer), FirCallableReferenceBinding::Bound) => {
                let receiver = dispatch_receiver.ok_or_else(failed)?;
                captures.push(receiver);
                wrapper_parameters.push(outer.get());
                (Some(self.ir.add_expr(IrExpr::GetValue(0))), 1, 0)
            }
            (Some(_), FirCallableReferenceBinding::Unbound) if dispatch_receiver.is_none() => {
                let Some(_) = reference.params.first() else {
                    return Err(failed());
                };
                (Some(self.ir.add_expr(IrExpr::GetValue(0))), 0, 1)
            }
            (None, FirCallableReferenceBinding::Static | FirCallableReferenceBinding::Unbound)
                if dispatch_receiver.is_none() =>
            {
                (None, 0, 0)
            }
            _ => return Err(failed()),
        };

        let arguments = if let Some(adaptation) = adaptation {
            if adaptation.arguments.len() != parameters.len() {
                return Err(failed());
            }
            adaptation
                .arguments
                .iter()
                .enumerate()
                .map(|(parameter, argument)| {
                    let parameter = u32::try_from(parameter)
                        .expect("too many constructor-reference parameters");
                    Ok(match argument {
                        FirAdaptedReferenceArgument::Value(source)
                            if reference.params.get(*source as usize).is_some() =>
                        {
                            crate::ir::IrCheckedArgument::Expression {
                                parameter,
                                value: self.ir.add_expr(IrExpr::GetValue(own_start + *source)),
                            }
                        }
                        FirAdaptedReferenceArgument::Default => {
                            crate::ir::IrCheckedArgument::Default { parameter }
                        }
                        FirAdaptedReferenceArgument::Vararg {
                            values,
                            whole_array,
                        } => {
                            let mut elements = Vec::with_capacity(values.len());
                            for source in values.iter().copied() {
                                if reference.params.get(source as usize).is_none() {
                                    return Err(failed());
                                }
                                elements.push((
                                    self.ir.add_expr(IrExpr::GetValue(own_start + source)),
                                    *whole_array,
                                ));
                            }
                            crate::ir::IrCheckedArgument::Vararg {
                                parameter,
                                array_type: parameters[parameter as usize].get(),
                                elements,
                            }
                        }
                        FirAdaptedReferenceArgument::Value(_) => {
                            return Err(failed());
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            if reference.params.len() != parameters.len() + identity_source_start {
                return Err(failed());
            }
            (0..parameters.len())
                .map(|parameter| crate::ir::IrCheckedArgument::Expression {
                    parameter: u32::try_from(parameter)
                        .expect("too many constructor-reference parameters"),
                    value: self.ir.add_expr(IrExpr::GetValue(
                        own_start
                            + u32::try_from(parameter + identity_source_start)
                                .expect("too many constructor-reference parameters"),
                    )),
                })
                .collect()
        };

        let enclosing_temporary = self.next_temporary;
        self.next_temporary = u32::try_from(captures.len() + reference.params.len())
            .expect("too many constructor-reference adapter parameters");
        let construction = self.constructor_reference_call(
            target,
            classifier,
            parameters,
            outer,
            outer_receiver,
            &arguments,
        )?;
        self.next_temporary = enclosing_temporary;
        let body = self.callable_reference_adapter_body(construction, result.get(), reference.ret);
        wrapper_parameters.extend(reference.params.iter().copied());
        let adapter_name = format!(
            "$fir_ctor_ref_{}_{}",
            self.body.owner().raw(),
            self.ir.functions.len()
        );
        let function = self.ir.add_fun(IrFunction {
            name: adapter_name.clone(),
            params: wrapper_parameters.clone(),
            ret: crate::types::stored_value_ty(reference.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir.private_methods.insert(function);
        if reflective {
            let capture = match captures.as_slice() {
                [] => None,
                [capture] => Some(*capture),
                _ => return Err(failed()),
            };
            return Ok(self.structural_constructor_reference(
                classifier,
                function,
                reference_ty,
                wrapper_parameters,
                capture,
            ));
        }
        self.ir
            .lambda_own_params_from
            .insert(function, captures.len() as u32);
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: function,
            arity,
            captures,
            sam: None,
            inline_body: None,
        }))
    }

    fn structural_constructor_reference(
        &mut self,
        classifier: TypeName,
        adapter: crate::ir::FunId,
        function_type: Ty,
        target_parameters: Vec<Ty>,
        capture: Option<ExprId>,
    ) -> ExprId {
        self.ir
            .add_expr(IrExpr::CallableReference(crate::ir::IrCallableReference {
                target: crate::ir::IrCallableReferenceTarget::Constructor { classifier },
                adapter,
                captures: Vec::new(),
                bound_receiver: capture,
                function_type,
                declaration_parameters: target_parameters.into_boxed_slice(),
                declaration_result: Ty::Unit,
                declaration_suspend: false,
                adaptation: None,
            }))
    }

    fn constructor_reference_call(
        &mut self,
        target: &crate::fir::FirConstructorTarget,
        classifier: TypeName,
        parameters: &[ResolvedTy],
        outer_parameter: Option<ResolvedTy>,
        outer_receiver: Option<ExprId>,
        arguments: &[crate::ir::IrCheckedArgument],
    ) -> Result<ExprId, FirLoweringFailure> {
        match target {
            crate::fir::FirConstructorTarget::Module(target) => {
                let primary_in_current_file = self
                    .index
                    .callable(*target)
                    .and_then(|callable| self.index.declaration_anchor(callable.declaration))
                    .is_some_and(|anchor| anchor.sibling == 0)
                    && self.ir.class_id_by_name(classifier).is_some();
                let parameter_types = parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let declaration_parameter_types = self
                    .index
                    .callable(*target)
                    .and_then(|callable| self.index.signature(callable.declaration))
                    .ok_or(FirLoweringFailure::MissingCallable(*target))?
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                self.module_constructor_call(ModuleConstructorRequest {
                    target: *target,
                    classifier,
                    argument_parameter_types: &parameter_types,
                    declaration_parameter_types: &declaration_parameter_types,
                    primary_in_current_file,
                    context_parameter_count: 0,
                    outer_receiver,
                    external_capture_arguments: None,
                    arguments,
                })
                .ok_or(FirLoweringFailure::UnsupportedModuleConstructor(*target))
            }
            crate::fir::FirConstructorTarget::External {
                declaration,
                classifier: selected_classifier,
                parameters: selected_parameters,
                annotation,
            } => {
                if *selected_classifier != classifier || selected_parameters.as_ref() != parameters
                {
                    return Err(FirLoweringFailure::UnsupportedExternalConstructor(
                        *declaration,
                    ));
                }
                self.external_constructor_call(
                    *declaration,
                    classifier,
                    parameters,
                    0,
                    outer_parameter,
                    outer_receiver,
                    arguments,
                    annotation.as_deref(),
                )
                .ok_or(FirLoweringFailure::UnsupportedExternalConstructor(
                    *declaration,
                ))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn checked_classifier_callable_reference(
        &mut self,
        classifier: TypeName,
        operation: FirClassifierCallable,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        binding: FirCallableReferenceBinding,
        adaptation: Option<&FirReferenceAdaptation>,
        reference_ty: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let failed = || FirLoweringFailure::UnsupportedClassifierCallableReference(classifier);
        if binding != FirCallableReferenceBinding::Static {
            return Err(failed());
        }
        let Ty::Fun(reference) = reference_ty.non_null() else {
            return Err(failed());
        };
        let arity = u8::try_from(reference.params.len()).map_err(|_| failed())?;
        let arguments = if let Some(adaptation) = adaptation {
            if adaptation.arguments.len() != parameters.len() {
                return Err(failed());
            }
            adaptation
                .arguments
                .iter()
                .map(|argument| match argument {
                    FirAdaptedReferenceArgument::Value(source)
                        if reference.params.get(*source as usize).is_some() =>
                    {
                        Ok(self.ir.add_expr(IrExpr::GetValue(*source)))
                    }
                    FirAdaptedReferenceArgument::Value(_)
                    | FirAdaptedReferenceArgument::Default
                    | FirAdaptedReferenceArgument::Vararg { .. } => Err(failed()),
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            if reference.params.len() != parameters.len()
                || reference
                    .params
                    .iter()
                    .copied()
                    .ne(parameters.iter().map(|parameter| parameter.get()))
            {
                return Err(failed());
            }
            (0..parameters.len())
                .map(|parameter| {
                    self.ir.add_expr(IrExpr::GetValue(
                        u32::try_from(parameter).expect("too many classifier-callable parameters"),
                    ))
                })
                .collect()
        };
        let call = match (operation, arguments.as_slice()) {
            (FirClassifierCallable::EnumValues, []) => {
                self.ir.add_expr(IrExpr::EnumValues { classifier })
            }
            (FirClassifierCallable::EnumValueOf, [argument]) => {
                self.ir.add_expr(IrExpr::EnumValueOf {
                    classifier,
                    arg: *argument,
                })
            }
            (FirClassifierCallable::ArrayConstructor { element }, [size]) => {
                self.array_construction_from_values(result, element, *size, None)?
            }
            (FirClassifierCallable::ArrayConstructor { element }, [size, initializer]) => self
                .array_construction_from_values(
                    result,
                    element,
                    *size,
                    Some((*initializer, parameters[1].get())),
                )?,
            (FirClassifierCallable::SamConstructor { conversion }, [function]) => self
                .sam_function_value_adapter(&conversion, *function)
                .ok_or_else(failed)?,
            _ => return Err(failed()),
        };
        let body = self.callable_reference_adapter_body(call, result.get(), reference.ret);
        let function = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_classifier_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: reference.params.clone(),
            ret: crate::types::stored_value_ty(reference.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir.private_methods.insert(function);
        self.ir.lambda_own_params_from.insert(function, 0);
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: function,
            arity,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        }))
    }
}
