//! Materialization of function-typed property references from checked accessor identities.

use crate::fir::{
    FirCallableReferenceBinding, FirPropertyReferenceTarget, FirPropertyTarget,
    FirReferenceAdaptation,
};
use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrFunction};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn materialize_property_function_reference(
        &mut self,
        target: &FirPropertyReferenceTarget,
        binding: FirCallableReferenceBinding,
        dispatch_capture: Option<ExprId>,
        extension_capture: Option<ExprId>,
        adaptation: Option<&FirReferenceAdaptation>,
        reference_ty: Ty,
    ) -> Result<Option<ExprId>, FirLoweringFailure> {
        let Ty::Fun(reference) = reference_ty.non_null() else {
            return Ok(None);
        };
        if adaptation.is_some_and(|adaptation| !adaptation.arguments.is_empty()) {
            return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
        }
        let arity = u8::try_from(reference.params.len())
            .map_err(|_| FirLoweringFailure::UnsupportedPropertyReferenceTarget)?;
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
        let mut captures = Vec::new();
        let mut capture_types = Vec::new();
        let captured_receiver = if let Some(value) = captured {
            let receiver =
                receiver_type.ok_or(FirLoweringFailure::UnsupportedPropertyReferenceTarget)?;
            captures.push(value);
            capture_types.push(receiver.get());
            Some(self.ir.add_expr(IrExpr::GetValue(0)))
        } else {
            None
        };
        let own_start = captures.len() as u32;
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
        let (dispatch_receiver, extension_receiver) = if target_is_extension {
            (None, selected_receiver)
        } else {
            (selected_receiver, None)
        };

        let read = match target {
            FirPropertyReferenceTarget::Module(target) => {
                self.ir
                    .add_expr(IrExpr::Checked(IrCheckedOperation::PropertyRead {
                        target: *target,
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments: Vec::new(),
                        substitutions: Vec::new(),
                    }))
            }
            FirPropertyReferenceTarget::SpecializedModule { property, .. } => {
                self.ir
                    .add_expr(IrExpr::Checked(IrCheckedOperation::PropertyRead {
                        target: *property,
                        dispatch_receiver,
                        extension_receiver,
                        context_arguments: Vec::new(),
                        substitutions: Vec::new(),
                    }))
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
                } = getter.as_ref()
                else {
                    return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
                };
                if !parameters.is_empty() {
                    return Err(FirLoweringFailure::UnsupportedPropertyReferenceTarget);
                }
                self.external_property_access(
                    *property,
                    dispatch.clone(),
                    *receiver,
                    parameters,
                    *result,
                    *extension_receiver_parameter,
                    dispatch_receiver,
                    extension_receiver,
                    &[],
                    false,
                )
                .ok_or(FirLoweringFailure::UnsupportedExternalProperty(*property))?
            }
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
        Ok(Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: function,
            arity,
            captures,
            sam: None,
            inline_body: None,
        })))
    }
}
