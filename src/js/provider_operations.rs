//! JavaScript realization of selected dependency declarations the target implements itself.
//!
//! Checked IR keeps every selected declaration as an ordinary dependency access. The provider
//! attached the operation a declaration denotes to its frozen facts, so a read of an array's
//! `size` or a string's `length` is realized here, at the target, from that fact and never from
//! a spelling.

use crate::backend::{BackendCompilerIntrinsic, CheckedBackendCallables};
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile, IrIntrinsic};

/// Realize each dependency property read whose getter is an array's `size` or a string's
/// `length` as the JavaScript length operation, and an enum's `name` as its stored field.
pub(super) fn realize(ir: &mut IrFile, callables: &CheckedBackendCallables) {
    for expression in &mut ir.exprs {
        let IrExpr::Checked(checked) = expression else {
            continue;
        };
        let IrCheckedOperation::ExternalPropertyRead {
            target,
            receiver: Some(receiver),
            arguments,
            result,
            ..
        } = &**checked
        else {
            continue;
        };
        if !arguments.is_empty() {
            continue;
        }
        let Some(property) = callables.property(*target) else {
            continue;
        };
        let operation = callables
            .callable(property.getter)
            .and_then(|getter| getter.compiler_operation());
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
