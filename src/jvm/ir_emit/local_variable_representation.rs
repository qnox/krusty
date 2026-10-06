//! JVM storage for source locals whose declaration omits an initializer.
//!
//! Common IR retains the declared semantic type and the parser-supplied zero placeholder. An inline
//! expansion may specialize the local's semantic type, but that does not change the JVM erasure of
//! the declaration that owns its slot. This boundary chooses both the erased slot and its matching
//! physical zero; common lowering never rewrites a semantic type to encode either decision.

use super::*;
use crate::ir::{ExprId, IrExpr};

/// The JVM type of the slot a `Variable` declaration owns. A declaration always stores a value, so
/// semantic `Unit` is the `kotlin/Unit` reference; only a callable's control-flow return is `V`.
/// Every reader of a declaration's representation asks here: the live slot map when the slot is
/// entered, and the body-wide declaration table ([`super::collect_body_var_types`]) once its scope has
/// closed or before it opens.
pub(super) fn slot_type(ir: &IrFile, declaration: ExprId, semantic: Ty) -> Ty {
    let declared = ir
        .deferred_local_types
        .get(&declaration)
        .copied()
        .unwrap_or(semantic);
    ir_ty_to_jvm(&stored_value_ty(declared))
}

fn emit_deferred_zero(
    ir: &IrFile,
    declaration: ExprId,
    initializer: ExprId,
    slot: Ty,
    code: &mut CodeBuilder,
    classes: &mut ClassWriter,
) -> bool {
    // Only the parser's synthetic default is replaced. A captured deferred local's holder
    // (`RefNew`) is real storage: substituting `null` makes the later `element` write throw.
    if !ir.deferred_local_types.contains_key(&declaration)
        || !matches!(ir.expr(initializer), IrExpr::Const(_))
    {
        return false;
    }
    push_zero(slot, code, classes);
    true
}

impl Emitter<'_> {
    pub(super) fn emit_local_variable(
        &mut self,
        declaration: ExprId,
        index: u32,
        semantic_ty: Ty,
        initializer: Option<ExprId>,
        named: bool,
        code: &mut CodeBuilder,
    ) {
        // A mutable captured local is represented explicitly by a `RefNew` initializer. Its source
        // type remains semantic, while the local slot stores the backend's holder.
        let slot_ty = initializer
            .filter(|initializer| matches!(self.ir.expr(*initializer), IrExpr::RefNew { .. }))
            .map(|initializer| self.value_ty(initializer))
            // A local declaration always owns a value slot. Semantic Unit is the singleton here;
            // only a callable control-flow return uses the JVM void representation.
            .unwrap_or_else(|| slot_type(self.ir, declaration, semantic_ty));

        // The synthetic zero on an inline-return result exists for common-IR definite assignment;
        // it is not a JVM store. Keep the semantic slot binding now so it survives the loop body's
        // scope restore, but defer occupying the physical frame slot until the first real store.
        // Locals inside the returned operand can then reuse this slot exactly as kotlinc does.
        if self.open_inline_return_frame(declaration, index, slot_ty) {
            let slot = self
                .frame
                .enter_deferred(super::frame_map::FrameKey::Value(index), slot_ty);
            self.slots.insert(index, (slot, slot_ty));
            self.unassigned_values.insert(index);
            return;
        }

        // A spilled local is declared twice with the same value identity. Reuse its slot when the
        // verifier types agree, or when both are references; never alias differing primitives.
        let is_reference = |ty: Ty| matches!(ty, Ty::String | Ty::Obj(..)) || ty.is_array();
        let reused = self
            .slots
            .get(&index)
            .copied()
            .filter(|(_, existing)| {
                *existing == slot_ty || (is_reference(*existing) && is_reference(slot_ty))
            })
            .map(|(slot, _)| slot);
        // Enter before evaluating the initializer, as kotlinc does. A call-operand holder is the
        // exception: its value is materialized first and the slot is entered afterwards.
        let holds_operand = self.ir.call_operand_bindings.contains(&declaration);
        let entered = reused.or_else(|| {
            (!holds_operand).then(|| self.enter_unassigned_value(index, slot_ty, false))
        });

        if let Some(cell) = initializer.and_then(|value| self.stored_shared_cell(value)) {
            let slot = entered
                .unwrap_or_else(|| self.enter_unassigned_value(index, slot_ty, holds_operand));
            self.unassigned_values.remove(&index);
            self.slots.insert(index, (slot, slot_ty));
            self.emit_shared_cell_declaration(declaration, slot, cell, code);
            return;
        }

        let slot = if let Some(initializer) = initializer {
            let source =
                if emit_deferred_zero(self.ir, declaration, initializer, slot_ty, code, self.cw) {
                    slot_ty
                } else {
                    // Only a source declaration runs kotlinc's `visitVariable` line restoration.
                    // Call-operand holders and backend suspension temps are unnamed even when a
                    // later migration changes which lowering pass creates them.
                    if named {
                        let previous = self.enter_declared_initializer(initializer);
                        let source = self.emit_consumed_operand(initializer, code);
                        self.leave_declared_initializer(previous);
                        source
                    } else {
                        self.emit_consumed_operand(initializer, code)
                    }
                };
            let semantic = self
                .ir
                .logical_types
                .get(&initializer)
                .copied()
                .unwrap_or(source);
            self.adapt_physical_operand(source, semantic, Some(semantic_ty), slot_ty, code);
            self.mark_expression_start(initializer, code);
            debug_lines::mark_statement(self.ir, declaration, code);
            let slot = entered
                .unwrap_or_else(|| self.enter_unassigned_value(index, slot_ty, holds_operand));
            self.unassigned_values.remove(&index);
            store(slot_ty, slot, code);
            self.mark_suspend_lambda_parameter_read(declaration, code);
            slot
        } else {
            self.unassigned_values.insert(index);
            entered.unwrap_or_else(|| self.enter_unassigned_value(index, slot_ty, holds_operand))
        };
        self.slots.insert(index, (slot, slot_ty));
        // A `lateinit` declaration emits no store, but its lexical debug lifetime still starts here.
        self.open_declared_local(declaration, slot, slot_ty, code);
    }
}
