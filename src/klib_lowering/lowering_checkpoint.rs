//! The common IR a lowering may leave behind, so a declined lowering leaves none.

use crate::ir::{ExprId, FunId, IrFile};

/// What a file held before a lowering began. Lowering appends functions and expressions and
/// records facts keyed by them; restoring removes exactly those.
pub(super) struct LoweringCheckpoint {
    expressions: usize,
    functions: usize,
    classes: usize,
}

impl LoweringCheckpoint {
    pub(super) fn of(ir: &IrFile) -> Self {
        Self {
            expressions: ir.exprs.len(),
            functions: ir.functions.len(),
            classes: ir.classes.len(),
        }
    }

    /// Remove what lowering appended, returning the first function id it had declared.
    pub(super) fn restore(self, ir: &mut IrFile) -> FunId {
        let first = ExprId::try_from(self.expressions).expect("an expression id fits u32");
        ir.exprs.truncate(self.expressions);
        ir.logical_types.retain(|expression, _| *expression < first);
        ir.construction_declared_params
            .retain(|expression, _| *expression < first);
        ir.checked_return_depths
            .retain(|expression, _| *expression < first);
        ir.binding_read_stability
            .retain(|expression, _| *expression < first);
        ir.callable_scopes.retain(|expression| *expression < first);
        ir.negations.retain(|expression| *expression < first);
        ir.short_circuits
            .retain(|expression, _| *expression < first);
        ir.written_casts.retain(|expression| *expression < first);
        ir.whens
            .exhaustive
            .retain(|expression, _| *expression < first);
        ir.external_singleton_dispatches
            .retain(|expression, _| *expression < first);
        let first = FunId::try_from(self.functions).expect("a function id fits u32");
        ir.functions.truncate(self.functions);
        ir.classes.truncate(self.classes);
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
        ir.fn_context_counts.retain(|function, _| *function < first);
        ir.singleton_extension_unread_receivers
            .retain(|function, _| *function < first);
        first
    }
}
