//! Materialization of function-typed property references from checked accessor identities.

use crate::fir::{
    FirCallableReferenceBinding, FirPropertyReferenceTarget, FirPropertyTarget,
    FirReferenceAdaptation, FirTypeSubstitution,
};
use crate::ir::{
    ExprId, IrCallableReference, IrCallableReferenceTarget, IrCheckedOperation, IrExpr, IrFunction,
    IrPropertyFunctionReference, IrTypeOp,
};
use crate::types::{Ty, TyKind};

use super::{BodyLowering, FirLoweringFailure};

/// The checked facts of a source property reference that lowering carries together: the selected
/// target, how it binds its receiver, and the type the reference is used as.
pub(super) struct CheckedPropertyReference<'a> {
    pub(super) target: &'a FirPropertyReferenceTarget,
    pub(super) binding: FirCallableReferenceBinding,
    pub(super) mutable: bool,
    pub(super) substitutions: &'a [FirTypeSubstitution],
    pub(super) adaptation: Option<&'a FirReferenceAdaptation>,
    pub(super) reference_ty: Ty,
}

impl BodyLowering<'_> {
    /// A property reference used as a function value is a callable reference whose adapter reads
    /// the property, so an inline call splices that read as it splices a function reference.
    pub(super) fn materialize_property_function_reference(
        &mut self,
        checked: &CheckedPropertyReference<'_>,
        dispatch_capture: Option<ExprId>,
        extension_capture: Option<ExprId>,
    ) -> Result<Option<ExprId>, FirLoweringFailure> {
        let CheckedPropertyReference {
            target,
            binding,
            mutable,
            substitutions,
            adaptation,
            reference_ty,
        } = *checked;
        let TyKind::Fun(reference) = reference_ty.non_null().kind() else {
            return Ok(None);
        };
        if adaptation.is_some_and(|adaptation| !adaptation.arguments.is_empty()) {
            return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
        }
        if dispatch_capture.is_some() && extension_capture.is_some() {
            return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
        }

        let (receiver_type, target_is_extension, property_type) = match target {
            FirPropertyReferenceTarget::Module(target) => {
                let property = self
                    .index
                    .property(*target)
                    .ok_or(FirLoweringFailure::MissingProperty(*target))?;
                let companion_extension = self
                    .index
                    .declaration_header(property.declaration)
                    .is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::COMPANION)
                    });
                let receiver = (!companion_extension)
                    .then_some(property.extension_receiver)
                    .flatten()
                    .or_else(|| {
                        self.index
                            .enclosing_classifier(property.declaration)
                            .map(|classifier| {
                                crate::fir::ResolvedTy::new(Ty::obj_name(classifier.classifier))
                                    .expect("a classifier identity is a publishable receiver type")
                            })
                    });
                let result = self
                    .index
                    .signature(property.declaration)
                    .ok_or(FirLoweringFailure::MissingProperty(*target))?
                    .result;
                (
                    receiver,
                    property.extension_receiver.is_some() && !companion_extension,
                    result,
                )
            }
            FirPropertyReferenceTarget::SpecializedModule {
                receiver,
                extension_receiver,
                property_type,
                ..
            } => (*receiver, *extension_receiver, *property_type),
            FirPropertyReferenceTarget::Classifier { .. } => return Ok(None),
            FirPropertyReferenceTarget::External {
                getter,
                extension_receiver,
                property_type,
                ..
            } => {
                let FirPropertyTarget::External { receiver, .. } = getter.as_ref() else {
                    return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
                };
                (*receiver, *extension_receiver, *property_type)
            }
        };

        let captured = dispatch_capture.or(extension_capture);
        let mut capture_types = Vec::new();
        let captured_receiver = if captured.is_some() {
            let receiver =
                receiver_type.ok_or(FirLoweringFailure::UnsupportedPropertyReferenceTarget)?;
            capture_types.push(receiver.get());
            Some(self.ir.add_expr(IrExpr::GetValue(0)))
        } else {
            None
        };
        let own_start = capture_types.len() as u32;
        let unbound_receiver = if binding == FirCallableReferenceBinding::Unbound {
            let expected =
                receiver_type.ok_or(FirLoweringFailure::UnsupportedPropertyReferenceTarget)?;
            if reference.params.as_slice() != [expected.get()] {
                return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
            }
            Some(self.ir.add_expr(IrExpr::GetValue(own_start)))
        } else {
            if receiver_type.is_some() && captured_receiver.is_none() {
                return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
            }
            if !reference.params.is_empty() {
                return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
            }
            None
        };
        let selected_receiver = captured_receiver.or(unbound_receiver);
        // A specialized reference reads the property through its declaration's accessors, which
        // are compiled against the declared receiver and result: the specialized receiver crosses
        // into the declared one, and the declared result back out, as a source read's do.
        let declared = match target {
            FirPropertyReferenceTarget::SpecializedModule {
                declared_receiver,
                declared_property_type,
                ..
            } => Some((*declared_receiver, *declared_property_type)),
            _ => None,
        };
        let selected_receiver = match (selected_receiver, declared, receiver_type) {
            (Some(value), Some((Some(declared), _)), Some(selected)) if target_is_extension => {
                Some(self.coerce_to_declared(value, selected.get(), declared.get()))
            }
            (receiver, ..) => receiver,
        };
        let (dispatch_receiver, extension_receiver) = if target_is_extension {
            (None, selected_receiver)
        } else {
            (selected_receiver, None)
        };

        let read = match target {
            FirPropertyReferenceTarget::Module(target) => self.ir.add_expr(IrExpr::Checked(
                Box::new(IrCheckedOperation::PropertyRead {
                    target: *target,
                    dispatch_receiver,
                    extension_receiver,
                    context_arguments: Vec::new(),
                    substitutions: Vec::new(),
                }),
            )),
            FirPropertyReferenceTarget::SpecializedModule {
                property,
                getter_inline_splice,
                ..
            } => {
                let read = self.ir.add_expr(IrExpr::Checked(Box::new(
                    IrCheckedOperation::PropertyRead {
                        target: *property,
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments: Vec::new(),
                        substitutions: super::checked::lower_substitutions(substitutions),
                    },
                )));
                self.record_inline_property_splice(read, getter_inline_splice);
                read
            }
            FirPropertyReferenceTarget::Classifier { .. } => {
                return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
            }
            FirPropertyReferenceTarget::External { getter, .. } => {
                let FirPropertyTarget::External {
                    property,
                    receiver,
                    parameters,
                    result,
                    extension_receiver_parameter,
                    dispatch,
                    metadata_constant_read: _,
                    semantic_role: _,
                } = getter.as_ref()
                else {
                    return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
                };
                if !parameters.is_empty() {
                    return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
                }
                self.external_property_access(super::source_calls::ExternalPropertyRequest {
                    target: *property,
                    dispatch: dispatch.clone(),
                    receiver_ty: *receiver,
                    parameters,
                    result: *result,
                    extension_receiver_parameter: *extension_receiver_parameter,
                    dispatch_receiver: super::source_calls::DispatchOperand::plain(
                        dispatch_receiver,
                    ),
                    extension_receiver,
                    arguments: &[],
                    write: false,
                })
                .ok_or(FirLoweringFailure::UnsupportedExternalProperty(*property))?
            }
        };
        // A module read is a checked operation whose result is the property's declared type; it
        // names no type of its own, so its producer records it, as every source read's does.
        if matches!(
            target,
            FirPropertyReferenceTarget::Module(_)
                | FirPropertyReferenceTarget::SpecializedModule { .. }
        ) {
            let declared_type = declared.map_or(property_type, |(_, declared)| declared);
            self.ir.set_logical_type(read, declared_type.get());
        }
        let read = match declared {
            Some((_, declared)) if declared != property_type => self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: read,
                type_operand: property_type.get(),
            }),
            _ => read,
        };
        let body = self.callable_reference_adapter_body(read, property_type.get(), reference.ret);
        let mut parameters = capture_types;
        parameters.extend(reference.params.iter().copied());
        let function = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_property_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: crate::types::stored_value_ty(reference.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir
            .set_method_visibility(function, crate::types::Visibility::Private);
        self.ir.lambda_own_params_from.insert(function, own_start);
        self.attach_generated_static_to_lexical_class(function);
        let declaration_parameters = target_is_extension
            .then_some(receiver_type)
            .flatten()
            .map(|receiver| receiver.get())
            .into_iter()
            .collect();
        Ok(Some(self.ir.add_expr(IrExpr::CallableReference(Box::new(
            IrCallableReference {
                target: IrCallableReferenceTarget::Property(Box::new(
                    IrPropertyFunctionReference {
                        target: target.clone(),
                        binding,
                        bound_extension_receiver: extension_capture.is_some(),
                        mutable,
                        substitutions: super::checked::lower_substitutions(substitutions),
                    },
                )),
                adapter: function,
                captures: Vec::new(),
                bound_receiver: captured,
                bound_contexts: 0,
                function_type: reference_ty,
                declaration_parameters,
                declaration_result: property_type.get(),
                declaration_suspend: false,
                adaptation: adaptation.cloned().map(Box::new),
                reflection_owner: None,
            },
        )))))
    }
}
