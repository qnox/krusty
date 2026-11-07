//! JavaScript realization of selected dependency declarations the target implements itself.
//!
//! Checked IR keeps every selected declaration as an ordinary dependency access. The provider
//! attached the operation a declaration denotes to its frozen facts, so a read of an array's
//! `size` is realized here, at the target, from that fact and never from a spelling.

use crate::backend::{BackendCompilerIntrinsic, CheckedBackendCallables};
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile, IrIntrinsic};

/// Realize each dependency property read whose getter is an array's `size` as the JavaScript
/// array length operation.
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
        let array_size = arguments.is_empty()
            && callables
                .property(*target)
                .and_then(|property| callables.callable(property.getter))
                .is_some_and(|getter| {
                    getter.compiler_operation() == Some(BackendCompilerIntrinsic::ArraySize)
                });
        if array_size {
            *expression = IrExpr::Call {
                callee: Box::new(Callee::Intrinsic {
                    operation: IrIntrinsic::ArraySize,
                    ret: *result,
                }),
                dispatch_receiver: Some(*receiver),
                args: Vec::new(),
            };
        }
    }
}
