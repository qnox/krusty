//! JVM realization of source property initializer stores.
//!
//! Common IR retains every Kotlin initializer because targets such as JavaScript do not provide
//! JVM-style zero-initialized instance fields. This pass removes only the exact declaration stores
//! whose values the JVM supplies implicitly; later assignments to the same field remain observable.

use crate::ir::{ExprId, IrExpr, IrFile, IrLocalPropertyLayout};

/// Select the JVM `@JvmField` storage realization for top-level declarations.
///
/// The package declaration carries the resolved annotation and stable property identity; the
/// common-IR layout carries that property's exact storage coordinate. Joining those identities
/// here avoids both syntax inspection and `(owner, name)` rebinding.
pub fn realize_top_level_jvm_fields(ir: &mut IrFile) {
    let fields = ir
        .package_properties
        .iter()
        .filter(|declaration| {
            crate::jvm::property_realizations::jvm_field_eligible(
                &declaration.annotations,
                declaration.visibility,
                declaration.flags,
                declaration.receiver.is_some(),
                !declaration.context_parameters.is_empty(),
            )
        })
        .filter_map(
            |declaration| match ir.local_property_layouts.get(&declaration.property) {
                Some(IrLocalPropertyLayout::TopLevelStorage { storage, .. }) => Some(*storage),
                Some(IrLocalPropertyLayout::TopLevelAccessor { .. }) | None => None,
                Some(
                    IrLocalPropertyLayout::Member { .. }
                    | IrLocalPropertyLayout::MemberExtension { .. },
                ) => unreachable!("a package property cannot have a member layout"),
            },
        )
        .collect::<Vec<_>>();
    for field in fields {
        ir.mark_jvm_field_static(field);
    }
}

/// Whether `expression` is a constant the JVM already supplies as a field's initial value (`null`,
/// zero of any width, `false`) — so a store of it is pure redundancy the emitter may drop. The rule
/// is Kotlin's, not the JVM's, and lives with the IR so every backend applies the same one.
pub(crate) fn is_jvm_default(ir: &IrFile, expression: ExprId) -> bool {
    ir.is_storage_default(expression)
}

/// Remove JVM-default declaration stores from constructor/init blocks.
///
/// The exact store identities come from common lowering. Matching only `(class, field, value)` would
/// also remove a later `init { property = 0 }`, which has different Kotlin semantics when a base
/// constructor has already dispatched to an override and written the field.
pub fn elide_default_property_stores(ir: &mut IrFile) {
    let elided: std::collections::HashSet<ExprId> = ir
        .property_initializer_stores
        .iter()
        .copied()
        .filter(|&store| ir.is_elided_initializer_store(store))
        .collect();

    if elided.is_empty() {
        return;
    }
    for expression in &mut ir.exprs {
        if let IrExpr::Block { stmts, .. } = expression {
            stmts.retain(|statement| !elided.contains(statement));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrConst;

    #[test]
    fn removes_only_the_recorded_declaration_store() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let declaration = ir.add_expr(IrExpr::SetField {
            receiver,
            class: 0,
            index: 0,
            value: zero,
        });
        let later_assignment = ir.add_expr(IrExpr::SetField {
            receiver,
            class: 0,
            index: 0,
            value: zero,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, later_assignment],
            value: None,
        });
        ir.property_initializer_stores.insert(declaration);

        elide_default_property_stores(&mut ir);

        let IrExpr::Block { stmts, .. } = ir.expr(body) else {
            panic!("body must remain a block");
        };
        assert_eq!(stmts, &[later_assignment]);
    }
}
