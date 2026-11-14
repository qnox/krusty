//! Join an already-selected property operation to its exact frozen accessor, without lookup.

use super::{BackendCallableFact, CheckedBackendCallables};
use crate::fir::ExternalCallableId;
use crate::ir::{Callee, ExprId, IrCheckedOperation, IrPropertyDispatch};
use crate::types::Ty;

pub struct DependencyAccessor<'a> {
    pub target: ExternalCallableId,
    pub callable: &'a BackendCallableFact,
    pub receiver: Option<ExprId>,
    pub arguments: &'a [ExprId],
    parameters: &'a [Ty],
    result: Ty,
    substitutions: &'a [crate::ir::IrCheckedSubstitution],
    dispatch: IrPropertyDispatch,
    write: bool,
}

impl CheckedBackendCallables {
    pub fn dependency_accessor<'a>(
        &'a self,
        operation: &'a IrCheckedOperation,
    ) -> Option<DependencyAccessor<'a>> {
        let (target, dispatch, receiver, arguments, parameters, result, substitutions, write) =
            match operation {
                IrCheckedOperation::ExternalPropertyRead {
                    target,
                    dispatch,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    substitutions,
                    ..
                } => (
                    *target,
                    *dispatch,
                    *receiver,
                    arguments,
                    parameters,
                    *result,
                    substitutions,
                    false,
                ),
                IrCheckedOperation::ExternalPropertyWrite {
                    target,
                    dispatch,
                    receiver,
                    arguments,
                    parameters,
                    result,
                    substitutions,
                    ..
                } => (
                    *target,
                    *dispatch,
                    *receiver,
                    arguments,
                    parameters,
                    *result,
                    substitutions,
                    true,
                ),
                _ => return None,
            };
        let property = self.property(target)?;
        let target = if write {
            property.setter?
        } else {
            property.getter
        };
        Some(DependencyAccessor {
            target,
            callable: self.callable(target)?,
            receiver,
            arguments,
            parameters,
            result,
            substitutions,
            dispatch,
            write,
        })
    }
}

impl DependencyAccessor<'_> {
    /// Whether the operation writes the property, through its setter.
    pub fn writes(&self) -> bool {
        self.write
    }

    /// Whether the operation dispatches on its receiver rather than naming a superclass's
    /// implementation.
    pub fn dispatches(&self) -> bool {
        self.dispatch == IrPropertyDispatch::Ordinary
    }

    pub fn callee(&self) -> Result<Callee, &'static str> {
        if self.dispatch != IrPropertyDispatch::Ordinary {
            return Err("a dependency accessor export called through super");
        }
        let generic_count = self
            .callable
            .generic_sig
            .as_ref()
            .map_or(0, |signature| signature.formals.len());
        if generic_count != self.substitutions.len()
            || self
                .substitutions
                .iter()
                .enumerate()
                .any(|(ordinal, substitution)| {
                    substitution.parameter
                        != (crate::fir::FirTypeParameterRef::External {
                            callable: self.target,
                            ordinal: ordinal as u32,
                        })
                })
        {
            return Err("a dependency property has incomplete checked generic substitutions");
        }
        let extension_receiver_parameter = if self.callable.is_member_extension()
            || (self.callable.is_extension() && self.receiver.is_none())
        {
            Some(
                u32::try_from(self.callable.context_count)
                    .map_err(|_| "an invalid accessor receiver coordinate")?,
            )
        } else {
            None
        };
        Ok(Callee::External {
            target: self.target,
            default_provider: None,
            params: self.parameters.to_vec(),
            ret: self.result,
            substitutions: self.substitutions.to_vec(),
            defaults: Vec::new(),
            extension_receiver_parameter,
        })
    }
}
