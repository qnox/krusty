//! JVM realization of a source local declared without an initializer.
//!
//! Inline substitution changes the local's semantic type, but kotlinc keeps the declaration's
//! original erased slot. Common IR records both facts; this pass commits the variable node to the
//! JVM slot type before representation-sensitive backend passes inspect local declarations.

use crate::ir::{IrExpr, IrFile};
use crate::types::stored_value_ty;

pub(super) fn realize(ir: &mut IrFile) {
    let declarations = ir
        .deferred_local_types
        .iter()
        .map(|(&expression, &declared)| (expression, declared))
        .collect::<Vec<_>>();
    for (expression, declared) in declarations {
        let IrExpr::Variable { ty, .. } = &mut ir.exprs[expression as usize] else {
            unreachable!("deferred-local provenance must name its variable declaration")
        };
        *ty = crate::jvm::ir_emit::ir_ty_to_jvm(&stored_value_ty(declared));
    }
}
