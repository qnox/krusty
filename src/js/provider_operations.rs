//! JavaScript realization of selected dependency declarations the target implements itself.
//!
//! Checked IR keeps every selected declaration as an ordinary dependency access. The provider
//! attached the operation a declaration denotes to its frozen facts, so a read of an array's
//! `size` or a string's `length`, and a call of an array's `get`/`set` or a string's `get`, is
//! realized here, at the target, from that fact and never from a spelling.

use crate::backend::{BackendCompilerIntrinsic, CheckedBackendCallables};
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile, IrIntrinsic};

/// Realize each dependency property read whose getter is an array's `size` or a string's
/// `length` as the JavaScript length operation, an enum's `name` as its stored field, and each
/// dependency call of an array's `get`/`set` or a string's `get` as the JavaScript element access.
pub(super) fn realize(ir: &mut IrFile, callables: &CheckedBackendCallables) {
    for expression in &mut ir.exprs {
        if let IrExpr::Call { callee, .. } = expression {
            if let Callee::External {
                target,
                ret,
                defaults,
                ..
            } = &**callee
            {
                let operation = match callables
                    .callable(*target)
                    .and_then(|callable| callable.compiler_operation())
                {
                    Some(BackendCompilerIntrinsic::ArrayGet) => Some(IrIntrinsic::ArrayGet),
                    Some(BackendCompilerIntrinsic::ArraySet) => Some(IrIntrinsic::ArraySet),
                    Some(BackendCompilerIntrinsic::StringGet) => Some(IrIntrinsic::StringGet),
                    _ => None,
                };
                if let Some(operation) = operation.filter(|_| defaults.is_empty()) {
                    let ret = *ret;
                    **callee = Callee::Intrinsic { operation, ret };
                }
            }
            continue;
        }
        let IrExpr::Checked(checked) = expression else {
            continue;
        };
        let IrCheckedOperation::ExternalPropertyRead {
            target,
            receiver: Some(receiver),
            arguments,
            result,
            substitutions,
            ..
        } = &**checked
        else {
            continue;
        };
        // A provider-selected generic accessor has a checked external-call path which carries its
        // declaration substitutions. The intrinsic rewrite below has no generic-call frame, so
        // leave those operations intact for that path.
        if !arguments.is_empty() || !substitutions.is_empty() {
            continue;
        }
        let Some(property) = callables.property(*target) else {
            continue;
        };
        let Some(getter) = callables.callable(property.getter) else {
            continue;
        };
        if getter
            .generic_sig
            .as_ref()
            .is_some_and(|signature| !signature.formals.is_empty())
        {
            continue;
        }
        let operation = getter.compiler_operation();
        let receiver = *receiver;
        let ty = *result;
        let intrinsic = |operation| IrExpr::Call {
            callee: Box::new(Callee::Intrinsic { operation, ret: ty }),
            dispatch_receiver: Some(receiver),
            args: Vec::new(),
        };
        match operation {
            Some(BackendCompilerIntrinsic::ArraySize) => {
                *expression = intrinsic(IrIntrinsic::ArraySize);
            }
            Some(BackendCompilerIntrinsic::StringLength) => {
                *expression = intrinsic(IrIntrinsic::StringLength);
            }
            // A JavaScript enum entry stores the `name` its base class declares as a plain field.
            Some(BackendCompilerIntrinsic::EnumName) => {
                *expression = IrExpr::PropertyRead {
                    receiver: Some(receiver),
                    owner: property.owner,
                    name: property.name.to_string(),
                    ty,
                    interface: false,
                    operation: None,
                };
            }
            _ => {}
        }
    }
}
