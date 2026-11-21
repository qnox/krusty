//! The common IR a lowering may leave behind, so a declined lowering leaves none.

use crate::ir::{ExprId, FunId, IrFile};

/// What a file held before a lowering began. Lowering appends functions and expressions and
/// records facts keyed by them, and may append members to a class it materialized before;
/// restoring removes exactly those.
pub(super) struct LoweringCheckpoint {
    expressions: usize,
    functions: usize,
    classes: usize,
    /// Each existing class's method and secondary-constructor counts.
    class_members: Vec<(usize, usize)>,
    statics: usize,
    class_self_types: crate::fx_hash::HashSet<crate::types::TypeName>,
}

impl LoweringCheckpoint {
    pub(super) fn of(ir: &IrFile) -> Self {
        Self {
            expressions: ir.exprs.len(),
            functions: ir.functions.len(),
            classes: ir.classes.len(),
            class_members: ir
                .classes
                .iter()
                .map(|class| (class.methods.len(), class.secondary_ctors.len()))
                .collect(),
            statics: ir.statics.len(),
            class_self_types: ir.declared_class_self_type_owners(),
        }
    }

    /// Remove what lowering appended, returning the first function id it had declared.
    pub(super) fn restore(self, ir: &mut IrFile) -> FunId {
        let first = ExprId::try_from(self.expressions).expect("an expression id fits u32");
        ir.exprs.truncate(self.expressions);
        crate::ir::truncate_expression_facts(ir, first);
        ir.statics.truncate(self.statics);
        ir.callable_reference_adapter_results
            .retain(|body, result| *body < first && *result < first);
        // The exhaustive classifier above prunes inherited and operation-owned expression facts.
        let first = FunId::try_from(self.functions).expect("a function id fits u32");
        let removed = ir.classes[self.classes..]
            .iter()
            .map(|class| class.fq_name_id())
            .collect::<Vec<_>>();
        ir.functions.truncate(self.functions);
        ir.classes.truncate(self.classes);
        for (class, (methods, secondaries)) in ir.classes.iter_mut().zip(self.class_members) {
            class.methods.truncate(methods);
            class.secondary_ctors.truncate(secondaries);
        }
        // An object literal's naming provenance is keyed by its class id, which a later class may
        // reuse; a stale entry would also count toward the next anonymous ordinal.
        let first_class = crate::ir::ClassId::try_from(self.classes).expect("a class id fits u32");
        ir.local_class_name_provenance
            .retain(|class, _| *class < first_class);
        for class in removed {
            ir.classifier_hierarchies.remove(&class);
            ir.function_overrides.remove(&class);
            ir.property_overrides.remove(&class);
            ir.classifier_hierarchies.remove(&class);
        }
        ir.restore_declared_class_self_type_owners(&self.class_self_types);
        ir.class_method_owners
            .retain(|function, _| *function < first);
        ir.fn_source_names.retain(|function, _| *function < first);
        ir.fn_params.retain(|function, _| *function < first);
        ir.lambda_own_params_from
            .retain(|function, _| *function < first);
        ir.method_visibilities
            .retain(|function, _| *function < first);
        ir.extension_receiver_fns
            .retain(|function| *function < first);
        ir.signatures.retain(|function, _| *function < first);
        ir.inline_fns.retain(|function| *function < first);
        ir.inline_only_fns.retain(|function| *function < first);
        ir.truncate_declared_parameters(first);
        ir.singleton_extension_unread_receivers
            .retain(|function, _| *function < first);
        first
    }
}

#[cfg(test)]
mod tests {
    use super::LoweringCheckpoint;
    use crate::ir::{IrConst, IrExpr, IrFile, IrIntrinsicSuspensionPoint};
    use crate::types::Ty;

    #[test]
    fn rollback_removes_suspension_facts_before_reusing_expression_ids() {
        let mut ir = IrFile::default();
        ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let checkpoint = LoweringCheckpoint::of(&ir);
        let appended = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        ir.suspend_calls.insert(appended, Ty::String);
        ir.intrinsic_suspension_points
            .insert(appended, IrIntrinsicSuspensionPoint { result: Ty::String });

        checkpoint.restore(&mut ir);

        assert!(!ir.suspend_calls.contains_key(&appended));
        assert!(!ir.intrinsic_suspension_points.contains_key(&appended));
        let reused = ir.add_expr(IrExpr::Const(IrConst::Int(2)));
        assert_eq!(reused, appended);
        assert!(!ir.suspend_calls.contains_key(&reused));
        assert!(!ir.intrinsic_suspension_points.contains_key(&reused));
    }

    #[test]
    fn rollback_forgets_an_inline_function_a_declined_lowering_declared() {
        let mut ir = IrFile::default();
        let checkpoint = LoweringCheckpoint::of(&ir);
        let declared = ir.functions.len() as crate::ir::FunId;
        ir.inline_fns.insert(declared);
        ir.inline_only_fns.insert(declared);

        checkpoint.restore(&mut ir);

        // A retry reuses the id for an ordinary callable, which must stay emitted.
        assert!(!ir.inline_fns.contains(&declared));
        assert!(!ir.inline_only_fns.contains(&declared));
    }
}
