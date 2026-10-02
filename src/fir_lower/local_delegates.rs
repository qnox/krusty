//! Common-IR handoff for checked local delegated properties.
//!
//! The result is an expression template plus semantic operands and source lifting provenance. It is
//! intentionally not an `IrFunction`; targets decide whether and how to materialize a helper.

use crate::fir::{
    DeclarationId, FirCallTarget, FirDelegateCall, FirDelegateDispatchReceiver, FirExprId,
    FirExprKind,
};
use crate::ir::{
    IrCheckedArgument, IrExpr, IrGeneratedParameterRole, IrLocalPropertyReference,
    IrParameterIdentity, IrTypeOp,
};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

struct LocalDelegateAccessorRequest<'a> {
    storage_name: &'a str,
    storage_type: Ty,
    result: Ty,
    reference: &'a IrLocalPropertyReference,
    reference_type: Ty,
    call: &'a FirDelegateCall,
    dispatch_parameter: Option<&'a crate::fir::FirLocalDelegateDispatchParameter>,
    value_type: Option<Ty>,
    site: crate::fir::FirLiftingSite,
    line: u32,
}

impl BodyLowering<'_> {
    pub(super) fn prepare_local_delegate_plans(&mut self) -> Result<(), FirLoweringFailure> {
        let plans = self.body.local_delegate_plans().to_vec();
        if plans.is_empty() {
            return Ok(());
        }
        for plan in plans {
            let plan_declaration = plan.declaration;
            let (reference, reference_type) = self.local_property_reference_plan(plan.reference)?;
            let getter = self.local_delegate_accessor(LocalDelegateAccessorRequest {
                storage_name: &plan.storage_name,
                storage_type: plan.storage_type.get(),
                result: plan.property_type.get(),
                reference: &reference,
                reference_type,
                call: &plan.get_value,
                dispatch_parameter: plan.get_value_dispatch.as_ref(),
                value_type: None,
                site: plan
                    .accessor_sites
                    .first()
                    .ok_or(FirLoweringFailure::InvalidLocalDelegatePlan(
                        self.body.owner(),
                    ))?
                    .clone(),
                line: plan.line,
            })?;
            let setter = plan
                .set_value
                .as_ref()
                .map(|call| {
                    self.local_delegate_accessor(LocalDelegateAccessorRequest {
                        storage_name: &plan.storage_name,
                        storage_type: plan.storage_type.get(),
                        result: Ty::Unit,
                        reference: &reference,
                        reference_type,
                        call,
                        dispatch_parameter: plan.set_value_dispatch.as_ref(),
                        value_type: Some(plan.property_type.get()),
                        site: plan
                            .accessor_sites
                            .get(1)
                            .ok_or(FirLoweringFailure::InvalidLocalDelegatePlan(
                                self.body.owner(),
                            ))?
                            .clone(),
                        line: plan.line,
                    })
                })
                .transpose()?;
            let id = u32::try_from(self.ir.local_delegate_plans.len())
                .map_err(|_| FirLoweringFailure::ValueIdentityOverflow)?;
            self.ir
                .local_delegate_plans
                .push(crate::ir::IrLocalDelegatePlan {
                    inline_declaration: self.inline_declaration,
                    declaration_lambda: self.declaration_lambda,
                    reference,
                    storage_name: plan.storage_name,
                    getter,
                    setter,
                });
            if self
                .ir
                .local_delegate_plan_ids
                .insert(plan_declaration, id)
                .is_some()
            {
                return Err(FirLoweringFailure::InvalidLocalDelegatePlan(
                    self.body.owner(),
                ));
            }
        }
        Ok(())
    }

    fn local_delegate_accessor(
        &mut self,
        request: LocalDelegateAccessorRequest<'_>,
    ) -> Result<crate::ir::IrLocalDelegateAccessorPlan, FirLoweringFailure> {
        let LocalDelegateAccessorRequest {
            storage_name,
            storage_type,
            result,
            reference,
            reference_type,
            call,
            dispatch_parameter,
            value_type,
            site,
            line,
        } = request;
        let source_order = reference.member_order;
        // Each convention operand is a distinct use. Sharing one expression identity would let an
        // inline accessor's unread-operand marker alias into the provider or the other accessor.
        let reference = self
            .ir
            .add_expr(IrExpr::LocalPropertyReference(reference.clone()));
        // The enclosing instance occupies slot 0 when the convention is a member extension.
        // The delegate follows it, which is also slot 0 when there is no dispatch receiver.
        let dispatch_type = call.dispatch_receiver.as_ref().and_then(dispatch_type);
        let delegate_slot = u32::from(dispatch_type.is_some());
        let delegate = self.ir.add_expr(IrExpr::GetValue(delegate_slot));
        let dispatch = dispatch_type.map(|_| self.ir.add_expr(IrExpr::GetValue(0)));
        let value_slot = delegate_slot + 1;
        let owner = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let mut arguments = vec![(owner, Ty::Null), (reference, reference_type)];
        if let Some(value_type) = value_type {
            arguments.push((self.ir.add_expr(IrExpr::GetValue(value_slot)), value_type));
        }
        let declaration = DeclarationId::from_raw(self.body.owner().raw());
        let mut body =
            self.local_delegate_call(declaration, call, delegate, dispatch, arguments, line)?;
        if result != Ty::Unit && call.result.get() != result {
            body = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: body,
                type_operand: result,
            });
        }
        let mut parameters = Vec::new();
        let mut identities = Vec::new();
        let mut captured_receivers = Vec::new();
        if let Some(dispatch_type) = dispatch_type {
            parameters.push(dispatch_type);
            match dispatch_parameter {
                Some(crate::fir::FirLocalDelegateDispatchParameter::ImplicitReceiver(receiver)) => {
                    identities.push(IrParameterIdentity::captured_receiver(0));
                    captured_receivers.push(super::lower_captured_receiver(receiver));
                }
                Some(crate::fir::FirLocalDelegateDispatchParameter::ContextValue(name)) => {
                    identities.push(IrParameterIdentity::captured_value(
                        Some(name.to_string()),
                        0,
                        crate::ir::IrValueCapture {
                            declaration: crate::ir::IrCapturedDeclaration::Parameter,
                            capturer: crate::ir::IrCapturingCallable::LocalFunction,
                        },
                    ));
                }
                None => {
                    return Err(FirLoweringFailure::InvalidLocalDelegatePlan(
                        self.body.owner(),
                    ));
                }
            }
        } else if dispatch_parameter.is_some() {
            return Err(FirLoweringFailure::InvalidLocalDelegatePlan(
                self.body.owner(),
            ));
        }
        parameters.push(storage_type);
        identities.push(IrParameterIdentity::generated(
            IrGeneratedParameterRole::LocalDelegateStorage,
            Some(storage_name.to_owned()),
        ));
        if let Some(value_type) = value_type {
            parameters.push(value_type);
            identities.push(IrParameterIdentity::property_setter_value());
        }
        let mut type_parameters = Vec::new();
        for ty in parameters.iter().copied().chain(std::iter::once(result)) {
            for parameter in super::generics::named_type_parameters(self.index, ty) {
                if !type_parameters
                    .iter()
                    .any(|present: &crate::ir::IrTypeParameter| {
                        present.semantic_name == parameter.semantic_name
                    })
                {
                    type_parameters.push(parameter);
                }
            }
        }
        Ok(crate::ir::IrLocalDelegateAccessorPlan {
            body,
            parameters,
            parameter_identities: identities,
            type_parameters,
            captured_receivers,
            result,
            source_order,
            site,
            line,
        })
    }

    fn local_property_reference_plan(
        &self,
        reference: FirExprId,
    ) -> Result<(IrLocalPropertyReference, Ty), FirLoweringFailure> {
        let expression =
            self.body
                .expr(reference)
                .ok_or(FirLoweringFailure::InvalidLocalDelegatePlan(
                    self.body.owner(),
                ))?;
        let reference_type = expression.ty.get();
        let FirExprKind::LocalPropertyReference {
            name,
            property_type,
            declaration,
            mutable,
            ordinal,
        } = &expression.kind
        else {
            return Err(FirLoweringFailure::InvalidLocalDelegatePlan(
                self.body.owner(),
            ));
        };
        let reference = self.local_property_reference(
            *declaration,
            (name, property_type.get()),
            (*mutable, *ordinal),
        )?;
        Ok((reference, reference_type))
    }

    fn local_delegate_call(
        &mut self,
        declaration: DeclarationId,
        call: &FirDelegateCall,
        delegate: crate::ir::ExprId,
        dispatch: Option<crate::ir::ExprId>,
        arguments: Vec<(crate::ir::ExprId, Ty)>,
        source_line: u32,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let dispatch = match (&call.dispatch_receiver, dispatch) {
            (Some(FirDelegateDispatchReceiver::Singleton { classifier, .. }), None) => {
                Some(self.ir.add_expr(IrExpr::SingletonValue {
                    classifier: *classifier,
                }))
            }
            (_, dispatch) => dispatch,
        };
        if let FirCallTarget::Module(target) = &call.target {
            let target = *target;
            let inline = {
                let callable = self
                    .index
                    .callable(target)
                    .ok_or(FirLoweringFailure::MissingCallable(target))?;
                callable.is_inline()
            };
            if !inline {
                return super::delegated_properties::delegated_call_with_dispatch(
                    self.index,
                    self.ir,
                    declaration,
                    call,
                    delegate,
                    dispatch,
                    arguments,
                )
                .map_err(|_| FirLoweringFailure::InvalidLocalDelegatePlan(self.body.owner()));
            }
            let checked = arguments
                .iter()
                .enumerate()
                .map(|(parameter, &(value, _))| {
                    Ok(IrCheckedArgument::Expression {
                        parameter: u32::try_from(parameter)
                            .map_err(|_| FirLoweringFailure::ValueIdentityOverflow)?,
                        value,
                    })
                })
                .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
            let selected_parameters = call
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let (dispatch_receiver, extension_receiver) = if call.extension {
                (dispatch, Some(delegate))
            } else {
                (Some(delegate), None)
            };
            let expression = self
                .same_file_call(
                    target,
                    super::source_calls::DispatchOperand::plain(dispatch_receiver),
                    extension_receiver,
                    &checked,
                    &selected_parameters,
                    &call.substitutions,
                    Some(source_line),
                )
                .ok_or(FirLoweringFailure::MissingCallable(target))??;
            return Ok(expression);
        }
        super::delegated_properties::delegated_call_with_dispatch(
            self.index,
            self.ir,
            declaration,
            call,
            delegate,
            dispatch,
            arguments,
        )
        .map_err(|_| FirLoweringFailure::InvalidLocalDelegatePlan(self.body.owner()))
    }
}

fn dispatch_type(receiver: &FirDelegateDispatchReceiver) -> Option<Ty> {
    match receiver {
        FirDelegateDispatchReceiver::Scoped { ty, .. }
        | FirDelegateDispatchReceiver::ContextBinding { ty, .. } => Some(ty.get()),
        FirDelegateDispatchReceiver::Singleton { .. } => None,
    }
}
