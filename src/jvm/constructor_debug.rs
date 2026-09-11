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

pub(super) fn property_line(ir: &IrFile, class: &IrClass, field: &str) -> Option<u32> {
    ir.prop_decl_lines
        .get(&(class.fq_name_id(), field.to_string()))
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
                let field = &class.fields[*index as usize].name;
                PropertyStore {
                    expression,
                    line: property_line(ir, class, field),
                }
            })
            .collect(),
    )
}
