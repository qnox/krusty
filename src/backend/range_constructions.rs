//! Range constructions on a target that does not lay out the range class.
//!
//! Checked IR keeps `start..end` as a range construction beside the declaration selected for it
//! (`Int.rangeTo`), so a target that lays out the range classes itself builds the range directly.
//! A target that does not builds it as kotlinc does: by calling that declaration, whose library
//! body constructs the range. The call is an ordinary dependency call from here on, so support
//! emission selects its body and the target calls it like any other.

use super::CheckedBackendCallables;
use crate::ir::{Callee, IrCheckedOperation, IrExpr, IrFile};
use crate::libraries::ExternalCallableKind;
use crate::types::TypeName;

/// Turn every range construction whose class `represents` rejects into a call of its declaration.
pub(crate) fn realize(
    ir: &mut IrFile,
    callables: &CheckedBackendCallables,
    represents: &dyn Fn(TypeName) -> bool,
) -> Result<(), String> {
    for expression in 0..ir.exprs.len() {
        let IrExpr::Checked(operation) = &ir.exprs[expression] else {
            continue;
        };
        let IrCheckedOperation::RangeConstruction {
            start,
            start_type,
            end,
            end_type,
            result,
            declaration,
            ..
        } = **operation
        else {
            continue;
        };
        let Some(class) = result.kotlin_class_internal() else {
            continue;
        };
        if represents(class) {
            continue;
        }
        let target =
            declaration.ok_or("a range construction has no declaration building its range")?;
        let callable = callables
            .callable(target)
            .ok_or("a range construction's declaration has no frozen fact")?;
        let (params, dispatch_receiver, args) = match callable.kind {
            ExternalCallableKind::Member => (vec![end_type], Some(start), vec![end]),
            ExternalCallableKind::Extension => (vec![start_type, end_type], None, vec![start, end]),
            _ => {
                return Err(
                    "a range construction's declaration is not a member or extension".into(),
                )
            }
        };
        ir.exprs[expression] = IrExpr::Call {
            callee: Box::new(Callee::External {
                target,
                default_provider: None,
                params,
                ret: result,
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            }),
            dispatch_receiver,
            args,
        };
    }
    Ok(())
}
