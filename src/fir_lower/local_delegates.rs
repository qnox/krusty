//! Common-IR handoff for checked local delegated properties.
//!
//! The result is an expression template plus semantic operands and source lifting provenance. It is
//! intentionally not an `IrFunction`; targets decide whether and how to materialize a helper.

use crate::fir::{
    DeclarationId, FirDelegateCall, FirDelegateDispatchReceiver, FirExprId, FirExprKind,
};
use crate::ir::{IrExpr, IrGeneratedParameterRole, IrParameterIdentity, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

struct LocalDelegateAccessorRequest<'a> {
    storage_name: &'a str,
    storage_type: Ty,
    result: Ty,
    reference: FirExprId,
    call: &'a FirDelegateCall,
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
        let declaration = DeclarationId::from_raw(self.body.owner().raw());
        let source = self
            .index
            .declaration_anchor(declaration)
            .map(|anchor| anchor.source)
            .ok_or(FirLoweringFailure::InvalidLocalDelegatePlan(
                self.body.owner(),
            ))?;
        for plan in plans {
            let plan_declaration = plan.declaration;
            let getter = self.local_delegate_accessor(LocalDelegateAccessorRequest {
                storage_name: &plan.storage_name,
                storage_type: plan.storage_type.get(),
                result: plan.property_type.get(),
                reference: plan.reference,
                call: &plan.get_value,
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
                        reference: plan.reference,
                        call,
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
                    source,
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
            call,
            value_type,
            site,
            line,
        } = request;
        // Each convention operand is a distinct use. Sharing one expression identity would let an
        // inline accessor's unread-operand marker alias into the provider or the other accessor.
        let (reference, reference_type) = self.fresh_local_property_reference(reference)?;
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
        let mut body = super::delegated_properties::delegated_call_with_dispatch(
            self.index,
            self.ir,
            declaration,
            call,
            delegate,
            dispatch,
            arguments,
        )
        .map_err(|_| FirLoweringFailure::InvalidLocalDelegatePlan(self.body.owner()))?;
        if result != Ty::Unit && call.result.get() != result {
            body = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: body,
                type_operand: result,
            });
        }
        let mut parameters = Vec::new();
        let mut identities = Vec::new();
        if let Some(dispatch_type) = dispatch_type {
            parameters.push(dispatch_type);
            identities.push(IrParameterIdentity::generated(
                IrGeneratedParameterRole::LocalDelegateDispatch,
                None,
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
        Ok(crate::ir::IrLocalDelegateAccessorPlan {
            body,
            parameters,
            parameter_identities: identities,
            result,
            site,
            line,
        })
    }

    fn fresh_local_property_reference(
        &mut self,
        reference: FirExprId,
    ) -> Result<(crate::ir::ExprId, Ty), FirLoweringFailure> {
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
        Ok((
            self.ir.add_expr(IrExpr::LocalPropertyReference(reference)),
            reference_type,
        ))
    }
}

fn dispatch_type(receiver: &FirDelegateDispatchReceiver) -> Option<Ty> {
    match receiver {
        FirDelegateDispatchReceiver::Scoped { ty, .. }
        | FirDelegateDispatchReceiver::ContextBinding { ty, .. } => Some(ty.get()),
        FirDelegateDispatchReceiver::Singleton { .. } => None,
    }
}
