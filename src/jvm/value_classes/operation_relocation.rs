//! Expression identity relocation when value-class lowering inserts a representation wrapper.
//!
//! The wrapper keeps the source expression identity, while the cloned inner node becomes the call
//! operation consumed by later JVM passes. Representation context is valid on both nodes; facts
//! whose identity denotes the call operation move exclusively to the clone.

use crate::ir::{ExprId, IrFile};

pub(super) fn clone_below_representation_wrapper(ir: &mut IrFile, source: ExprId) -> ExprId {
    let expression = ir.expr(source).clone();
    let target = ir.add_expr(expression);

    macro_rules! copy_fact {
        ($field:ident) => {
            if let Some(value) = ir.$field.get(&source).cloned() {
                ir.$field.insert(target, value);
            }
        };
    }
    copy_fact!(physical_types);
    copy_fact!(logical_types);
    copy_fact!(property_declaration_types);

    macro_rules! move_fact {
        ($field:ident) => {
            if let Some(value) = ir.$field.remove(&source) {
                ir.$field.insert(target, value);
            }
        };
    }
    // CPS and bytecode specialization consume the selected call, not its result wrapper.
    move_fact!(suspend_calls);
    move_fact!(value_class_suspend_calls);
    move_fact!(intrinsic_suspension_points);
    move_fact!(reified_call_subst);
    move_fact!(construction_declared_params);

    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrExpr;
    use crate::types::Ty;

    #[test]
    fn reified_substitutions_move_exclusively_to_the_cloned_call() {
        let mut ir = IrFile::default();
        let source = ir.add_expr(IrExpr::UnitInstance);
        let substitutions = vec![("T".to_owned(), Ty::String)];
        ir.reified_call_subst.insert(source, substitutions.clone());
        ir.logical_types.insert(source, Ty::String);

        let target = clone_below_representation_wrapper(&mut ir, source);

        assert!(!ir.reified_call_subst.contains_key(&source));
        assert_eq!(ir.reified_call_subst.get(&target), Some(&substitutions));
        assert_eq!(ir.logical_types.get(&source), Some(&Ty::String));
        assert_eq!(ir.logical_types.get(&target), Some(&Ty::String));
    }
}
