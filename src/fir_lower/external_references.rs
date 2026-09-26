//! Materialization of dependency callable references from provider-owned stable identities.

use crate::fir::{
    FirAdaptedReferenceArgument, FirCallableReferenceBinding, FirCallableReferenceTarget,
    FirReceiver, FirReferenceAdaptation,
};
use crate::ir::{ExprId, IrCheckedArgument, IrExpr, IrFunction};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    /// A dependency callable reference is an ordinary [`IrExpr::CallableReference`]: a generated
    /// adapter calls the provider-selected declaration, and the target backend chooses its carrier
    /// exactly as for a module declaration. A bound receiver is the adapter's first parameter.
    pub(super) fn checked_external_callable_reference(
        &mut self,
        target: FirCallableReferenceTarget,
        binding: FirCallableReferenceBinding,
        dispatch_receiver: Option<FirReceiver>,
        extension_receiver: Option<FirReceiver>,
        adaptation: Option<&FirReferenceAdaptation>,
        reference_ty: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let FirCallableReferenceTarget::External {
            declaration,
            default_provider,
            receiver,
            extension_receiver: target_is_extension,
            parameters,
            result,
            suspend: declaration_suspend,
        } = target
        else {
            unreachable!("module references stay on the module materializer")
        };
        let Ty::Fun(reference) = reference_ty.non_null() else {
            return Err(FirLoweringFailure::UnsupportedExternalCallableReference(
                declaration,
            ));
        };
        let dispatch_capture = dispatch_receiver
            .map(|receiver| self.expression_with_conversion(receiver.value, receiver.conversion))
            .transpose()?;
        let extension_capture = extension_receiver
            .map(|receiver| self.expression_with_conversion(receiver.value, receiver.conversion))
            .transpose()?;
        if dispatch_capture.is_some() && extension_capture.is_some() {
            return Err(FirLoweringFailure::UnsupportedExternalCallableReference(
                declaration,
            ));
        }

        let mut captures = Vec::new();
        let mut capture_types = Vec::new();
        let captured_receiver = dispatch_capture.or(extension_capture).map(|value| {
            let slot = captures.len() as u32;
            captures.push(value);
            capture_types.push(
                receiver
                    .expect("a checked bound reference has a receiver")
                    .get(),
            );
            slot
        });
        let own_start = captures.len() as u32;
        let mut own_parameter_slots = (0..reference.params.len() as u32).collect::<Vec<_>>();
        let unbound_receiver = if binding == FirCallableReferenceBinding::Unbound {
            receiver.ok_or_else(|| {
                FirLoweringFailure::UnsupportedExternalCallableReference(declaration)
            })?;
            if reference.params.is_empty() {
                return Err(FirLoweringFailure::UnsupportedExternalCallableReference(
                    declaration,
                ));
            }
            own_parameter_slots.remove(0);
            Some(self.ir.add_expr(IrExpr::GetValue(own_start)))
        } else {
            None
        };
        let selected_receiver = captured_receiver
            .map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)))
            .or(unbound_receiver);
        let (dispatch_receiver, extension_receiver) = if target_is_extension {
            (None, selected_receiver)
        } else {
            (selected_receiver, None)
        };

        let arguments = if let Some(adaptation) = adaptation {
            if adaptation.arguments.len() != parameters.len() {
                return Err(FirLoweringFailure::UnsupportedExternalCallableReference(
                    declaration,
                ));
            }
            adaptation
                .arguments
                .iter()
                .enumerate()
                .map(|(parameter, argument)| {
                    let parameter = parameter as u32;
                    Ok(match argument {
                        FirAdaptedReferenceArgument::Value(source) => {
                            if reference.params.get(*source as usize).is_none() {
                                return Err(
                                    FirLoweringFailure::UnsupportedExternalCallableReference(
                                        declaration,
                                    ),
                                );
                            }
                            IrCheckedArgument::Expression {
                                parameter,
                                value: self.ir.add_expr(IrExpr::GetValue(own_start + *source)),
                            }
                        }
                        FirAdaptedReferenceArgument::Default => {
                            IrCheckedArgument::Default { parameter }
                        }
                        FirAdaptedReferenceArgument::Vararg {
                            values,
                            whole_array,
                        } => {
                            let array_type = parameters[parameter as usize].get();
                            let elements = values
                                .iter()
                                .copied()
                                .map(|source| {
                                    reference.params.get(source as usize).map(|_| {
                                        (
                                            self.ir.add_expr(IrExpr::GetValue(own_start + source)),
                                            *whole_array,
                                        )
                                    })
                                })
                                .collect::<Option<Vec<_>>>()
                                .ok_or(FirLoweringFailure::UnsupportedExternalCallableReference(
                                    declaration,
                                ))?;
                            IrCheckedArgument::Vararg {
                                parameter,
                                array_type,
                                elements,
                            }
                        }
                    })
                })
                .collect::<Result<Vec<_>, FirLoweringFailure>>()?
        } else {
            if own_parameter_slots.len() != parameters.len() {
                return Err(FirLoweringFailure::UnsupportedExternalCallableReference(
                    declaration,
                ));
            }
            own_parameter_slots
                .into_iter()
                .enumerate()
                .map(|(parameter, source)| IrCheckedArgument::Expression {
                    parameter: parameter as u32,
                    value: self.ir.add_expr(IrExpr::GetValue(own_start + source)),
                })
                .collect()
        };
        // Call normalization belongs to the synthesized adapter method, whose capture and own
        // parameters occupy the prefix of its value-slot namespace. Do not allocate its spills in
        // the enclosing source body's namespace: those indices are interpreted afresh when the
        // adapter method is emitted.
        let enclosing_temporary = self.next_temporary;
        self.next_temporary = u32::try_from(captures.len() + reference.params.len())
            .expect("too many external callable-reference adapter parameters");
        let call = self.external_call(super::source_calls::ExternalCallRequest {
            target: declaration,
            default_provider,
            receiver_ty: receiver,
            declared_receiver: None,
            parameters: &parameters,
            result,
            declared_result: None,
            suspend: declaration_suspend,
            can_inline: false,
            inline_plan: None,
            substitutions: &[],
            extension_receiver_parameter: None,
            dispatch_receiver,
            extension_receiver,
            arguments: &arguments,
        });
        self.next_temporary = enclosing_temporary;
        let call = call.ok_or(FirLoweringFailure::UnsupportedExternalCallableReference(
            declaration,
        ))?;
        let body = self.callable_reference_adapter_body(call, result.get(), reference.ret);
        let mut wrapper_parameters = capture_types;
        wrapper_parameters.extend(reference.params.iter().copied());
        let function = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_external_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: wrapper_parameters,
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
        self.attach_generated_static_to_lexical_class(function);
        let bound_receiver = match captures.as_slice() {
            [] => None,
            [receiver] => Some(*receiver),
            _ => {
                return Err(FirLoweringFailure::UnsupportedExternalCallableReference(
                    declaration,
                ))
            }
        };
        Ok(self
            .ir
            .add_expr(IrExpr::CallableReference(crate::ir::IrCallableReference {
                target: crate::ir::IrCallableReferenceTarget::External {
                    declaration,
                    receiver: receiver.map(crate::fir::ResolvedTy::get),
                },
                adapter: function,
                captures: Vec::new(),
                bound_receiver,
                function_type: reference_ty,
                declaration_parameters: parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                declaration_result: result.get(),
                declaration_suspend,
                adaptation: adaptation.cloned().map(Box::new),
            })))
    }
}
