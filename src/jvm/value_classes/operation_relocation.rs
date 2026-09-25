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
    // These facts identify the selected call operation. The source expression is now the
    // representation wrapper, so leaving a copy there would both orphan the real call and let a
    // later consumer mistake the wrapper for the operation that owns the provider decision.
    move_fact!(ext_call_source_receiver);
    move_fact!(semantic_call_roles);
    move_fact!(call_declared_ret);
    move_fact!(call_declared_params);
    move_fact!(static_extension_receivers);
    move_fact!(call_materialized_lambda_params);

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

    #[test]
    fn selected_call_facts_move_exclusively_to_the_cloned_operation() {
        let mut ir = IrFile::default();
        let source = ir.add_expr(IrExpr::UnitInstance);
        ir.ext_call_source_receiver.insert(source, Ty::String);
        ir.semantic_call_roles.insert(
            source,
            crate::libraries::SemanticCallRole::KotlinAnyToString,
        );
        ir.call_declared_ret.insert(source, Ty::String);
        ir.call_declared_params
            .insert(source, vec![Ty::String].into_boxed_slice());
        ir.static_extension_receivers.insert(source, 0);
        ir.call_materialized_lambda_params
            .insert(source, vec![false, true].into_boxed_slice());

        let target = clone_below_representation_wrapper(&mut ir, source);

        assert!(!ir.ext_call_source_receiver.contains_key(&source));
        assert_eq!(ir.ext_call_source_receiver.get(&target), Some(&Ty::String));
        assert!(!ir.semantic_call_roles.contains_key(&source));
        assert_eq!(
            ir.semantic_call_roles.get(&target),
            Some(&crate::libraries::SemanticCallRole::KotlinAnyToString)
        );
        assert!(!ir.call_declared_ret.contains_key(&source));
        assert_eq!(ir.call_declared_ret.get(&target), Some(&Ty::String));
        assert!(!ir.call_declared_params.contains_key(&source));
        assert_eq!(
            ir.call_declared_params.get(&target).map(AsRef::as_ref),
            Some([Ty::String].as_slice())
        );
        assert_eq!(ir.static_extension_receivers.remove(&target), Some(0));
        assert!(!ir.static_extension_receivers.contains_key(&source));
        assert_eq!(
            ir.call_materialized_lambda_params
                .get(&target)
                .map(AsRef::as_ref),
            Some([false, true].as_slice())
        );
        assert!(!ir.call_materialized_lambda_params.contains_key(&source));
    }
}
