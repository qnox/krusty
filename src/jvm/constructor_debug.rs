//! Source-line planning for JVM constructor property stores.
//!
//! Common IR owns the stable property identity and declaration line; bytecode emission supplies the
//! physical program counter. This module keeps that boundary explicit and never reconstructs a line
//! from a field name or a surrounding class header.

use crate::ir::{ExprId, IrClass, IrExpr, IrFile};

pub(super) struct PropertyStore {
    pub(super) expression: ExprId,
    pub(super) line: Option<u32>,
}

/// The line a constructor's store of the exact `field` coordinate maps to.
///
/// A PRIMARY-CONSTRUCTOR property's store goes to the line its declaration starts on, annotations
/// included: kotlinc puts `@SerialName("x")` over `val x: Int` on the annotation's line, while the
/// property's own getter stays on the `val` line. A BODY property is not the same shape — its
/// initializer store stays on its own line, annotation or not — which is why only constructor
/// properties carry `constructor_store_line`. Body-property stores retain the older declaration-line
/// map until that broader debug-metadata migration moves onto exact field coordinates too.
pub(super) fn property_line(ir: &IrFile, class: &IrClass, field: u32) -> Option<u32> {
    let field = class.fields.get(field as usize)?;
    if field.constructor_store_line != 0 {
        return Some(field.constructor_store_line);
    }
    ir.prop_decl_lines
        .get(&(class.fq_name_id(), field.name.clone()))
        .copied()
        .filter(|&line| line != 0)
}

/// Return the ordered property stores only when lowering retained a pure `SetField` initializer
/// block. A mixed block must be emitted atomically; the backend does not guess which instructions
/// correspond to source properties.
pub(super) fn initializer_property_stores(
    ir: &IrFile,
    class: &IrClass,
    init_body: ExprId,
) -> Option<Vec<PropertyStore>> {
    let IrExpr::Block { stmts, value } = ir.expr(init_body) else {
        return None;
    };
    if value.is_some()
        || !stmts
            .iter()
            .all(|&statement| matches!(ir.expr(statement), IrExpr::SetField { .. }))
    {
        return None;
    }
    Some(
        stmts
            .iter()
            .map(|&expression| {
                let IrExpr::SetField { index, .. } = ir.expr(expression) else {
                    unreachable!("initializer was checked as a SetField block")
                };
                PropertyStore {
                    expression,
                    line: property_line(ir, class, *index),
                }
            })
            .collect(),
    )
}
