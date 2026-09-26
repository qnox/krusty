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
        .filter(|&store| is_elided_initializer_store(ir, store))
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

/// Is `expression` a declaration's initializer store that writes only what the fresh field already
/// holds, and so must not be emitted?
///
/// `var x = 0` in a class body stores nothing: kotlinc omits an initializer that writes the value
/// the JVM's zero-filled field already holds. The omission is observable, not an optimization: a
/// base-class constructor that dispatches to an override runs BEFORE the subclass's initializers,
/// so a value it wrote through that override survives exactly because the declaration's own store
/// was never emitted. A later `init { x = 0 }` is a different statement with a different meaning,
/// which is why the store's exact identity comes from `property_initializer_stores` rather than
/// from its shape. The field's type is read after value-class carrier realization, so it is the
/// physical slot the JVM zero-fills.
fn is_elided_initializer_store(ir: &IrFile, expression: ExprId) -> bool {
    ir.property_initializer_stores.contains(&expression)
        && matches!(ir.expr(expression), IrExpr::SetField { class, index, value, .. }
        if crate::jvm::fresh_storage::holds_fresh_value(
            ir,
            ir.classes[*class as usize].fields[*index as usize].ty,
            *value,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrConst;
    use crate::types::Ty;

    #[test]
    fn removes_only_the_recorded_declaration_store() {
        let mut ir = IrFile::default();
        let mut holder = crate::plugins::synthetic_class("fixture/Holder");
        holder
            .fields
            .push(crate::ir::IrField::new("count".to_string(), Ty::Int));
        ir.add_class(holder);
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

    /// A declaration's zero store into an `int` slot is elided; the same store into a boxed
    /// `Integer` slot is kept, since that slot's fresh value is `null`.
    #[test]
    fn a_boxed_slot_keeps_its_zero_store() {
        let mut ir = IrFile::default();
        let mut holder = crate::plugins::synthetic_class("fixture/Holder");
        holder
            .fields
            .push(crate::ir::IrField::new("count".to_string(), Ty::Int));
        holder.fields.push(crate::ir::IrField::new(
            "boxed".to_string(),
            Ty::nullable(Ty::Int),
        ));
        ir.add_class(holder);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let scalar = ir.add_expr(IrExpr::SetField {
            receiver,
            class: 0,
            index: 0,
            value: zero,
        });
        let boxed = ir.add_expr(IrExpr::SetField {
            receiver,
            class: 0,
            index: 1,
            value: zero,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![scalar, boxed],
            value: None,
        });
        ir.property_initializer_stores.insert(scalar);
        ir.property_initializer_stores.insert(boxed);

        elide_default_property_stores(&mut ir);

        let IrExpr::Block { stmts, .. } = ir.expr(body) else {
            panic!("body must remain a block");
        };
        assert_eq!(stmts, &[boxed]);
    }
}
