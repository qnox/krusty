//! Join an already-selected property operation to its exact frozen accessor, without lookup.

use super::{BackendCallableFact, CheckedBackendCallables};
use crate::fir::ExternalCallableId;
use crate::ir::{Callee, ExprId, IrCheckedOperation, IrPropertyDispatch};
use crate::types::Ty;

pub(crate) struct DependencyAccessor<'a> {
    pub(crate) target: ExternalCallableId,
    pub(crate) callable: &'a BackendCallableFact,
    pub(crate) receiver: Option<ExprId>,
    pub(crate) arguments: &'a [ExprId],
    parameters: &'a [Ty],
    result: Ty,
    dispatch: IrPropertyDispatch,
}

impl CheckedBackendCallables {
    pub(crate) fn dependency_accessor<'a>(
        &'a self,
        operation: &'a IrCheckedOperation,
    ) -> Option<DependencyAccessor<'a>> {
        let (target, dispatch, receiver, arguments, parameters, result, write) = match operation {
            IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                ..
            } => (
                *target, *dispatch, *receiver, arguments, parameters, *result, false,
            ),
            IrCheckedOperation::ExternalPropertyWrite {
                target,
                dispatch,
                receiver,
                arguments,
                parameters,
                result,
                ..
            } => (
                *target, *dispatch, *receiver, arguments, parameters, *result, true,
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
            dispatch,
        })
    }
}

impl DependencyAccessor<'_> {
    pub(crate) fn callee(&self) -> Result<Callee, &'static str> {
        if self.dispatch != IrPropertyDispatch::Ordinary {
            return Err("a dependency accessor export called through super");
        }
        if self
            .callable
            .generic_sig
            .as_ref()
            .is_some_and(|s| !s.formals.is_empty())
        {
            return Err("a dependency property without checked generic substitutions");
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
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter,
        })
    }
}
