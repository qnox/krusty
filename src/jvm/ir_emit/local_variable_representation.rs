//! JVM storage for source locals whose declaration omits an initializer.
//!
//! Common IR retains the declared semantic type and the parser-supplied zero placeholder. An inline
//! expansion may specialize the local's semantic type, but that does not change the JVM erasure of
//! the declaration that owns its slot. This boundary chooses both the erased slot and its matching
//! physical zero; common lowering never rewrites a semantic type to encode either decision.

use super::*;
use crate::ir::{ExprId, IrExpr};
use crate::types::TyKind;

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
    let slot = ir_ty_to_jvm(&stored_value_ty(declared));
    erased_inline_parameter_slot(ir, declaration, semantic).unwrap_or(slot)
}

/// The verifier type of an inlined type-parameter value.
///
/// kotlinc stores that parameter as its erased bound (`T : AutoCloseable?` is `AutoCloseable`) and
/// `checkcast`s back to the call-site type at each use. An unconstrained `T` stays the specialized
/// reference: its bound erases to `Object`, and no cast is emitted. A primitive specialization
/// stays unboxed.
fn erased_inline_parameter_slot(ir: &IrFile, declaration: ExprId, semantic: Ty) -> Option<Ty> {
    let declared = ir.inline_declared_type(declaration)?;
    if !declared.non_null().is_ty_param() {
        return None;
    }
    let erased = ir_ty_to_jvm(&stored_value_ty(declared));
    let specialized = ir_ty_to_jvm(&stored_value_ty(semantic));
    if !erased.is_reference() || !specialized.is_reference() || erased == specialized {
        return None;
    }
    let top = erased
        .obj_internal()
        .is_some_and(crate::jvm::jvm_class_map::is_jvm_erased_top);
    if top {
        None
    } else {
        Some(erased)
    }
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
        let is_reference =
            |ty: Ty| matches!(ty.kind(), TyKind::STRING | TyKind::Obj(..)) || ty.is_array();
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
            self.materialize_initializer(
                declaration,
                initializer,
                named,
                semantic_ty,
                slot_ty,
                code,
            );
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

    /// The holders of one inline lambda invocation's operands, which start `statements`; returns
    /// how many it bound. kotlinc's inliner finds every operand on the stack at the `invoke` and
    /// stores them last first, each into the next free slot, and the lambda's locals open once all
    /// are stored. A holder that is not a plain stored value is bound on its own.
    pub(super) fn emit_invocation_operand_bindings(
        &mut self,
        statements: &[ExprId],
        code: &mut CodeBuilder,
    ) -> usize {
        let operands: Vec<(ExprId, u32, Ty, ExprId, bool)> = statements
            .iter()
            .map_while(|&declaration| {
                if !self.ir.invocation_operand_bindings.contains(&declaration) {
                    return None;
                }
                let IrExpr::Variable {
                    index,
                    ty,
                    init: Some(initializer),
                    named,
                } = *self.ir.expr(declaration)
                else {
                    return None;
                };
                let plain = !self.slots.contains_key(&index)
                    && !matches!(self.ir.expr(initializer), IrExpr::RefNew { .. })
                    && self.stored_shared_cell(initializer).is_none();
                plain.then_some((declaration, index, ty, initializer, named))
            })
            .collect();
        // An operand that clears the stack (a `try`, a jump out of an enclosing loop, a suspension)
        // cannot run above the operands before it; such an invocation binds each operand in turn.
        let clears_prefix = operands
            .iter()
            .skip(1)
            .any(|&(_, _, _, initializer, _)| self.spills_operand_prefix(initializer));
        if operands.len() < 2 || clears_prefix {
            return 0;
        }
        let mut stored = Vec::with_capacity(operands.len());
        for &(declaration, index, semantic_ty, initializer, named) in &operands {
            let slot_ty = slot_type(self.ir, declaration, semantic_ty);
            self.materialize_initializer(
                declaration,
                initializer,
                named,
                semantic_ty,
                slot_ty,
                code,
            );
            stored.push((declaration, index, slot_ty));
        }
        for &(declaration, index, slot_ty) in stored.iter().rev() {
            let slot = self.enter_unassigned_value(index, slot_ty, true);
            self.unassigned_values.remove(&index);
            store(slot_ty, slot, code);
            self.mark_suspend_lambda_parameter_read(declaration, code);
            self.slots.insert(index, (slot, slot_ty));
        }
        for &(declaration, index, slot_ty) in &stored {
            let slot = self.slots[&index].0;
            self.open_declared_local(declaration, slot, slot_ty, code);
        }
        operands.len()
    }

    /// Leave `declaration`'s `initializer` on the stack as the slot type `slot_ty`, with the
    /// declaration's line marked as kotlinc's `visitVariable` marks it.
    fn materialize_initializer(
        &mut self,
        declaration: ExprId,
        initializer: ExprId,
        named: bool,
        semantic_ty: Ty,
        slot_ty: Ty,
        code: &mut CodeBuilder,
    ) {
        let source =
            if emit_deferred_zero(self.ir, declaration, initializer, slot_ty, code, self.cw) {
                slot_ty
            } else {
                // Only a source declaration runs kotlinc's `visitVariable` line restoration.
                // Call-operand holders and backend suspension temps are unnamed even when a
                // later migration changes which lowering pass creates them.
                if named {
                    let previous = self.enter_declared_initializer(initializer);
                    let declared_try = matches!(self.ir.expr(initializer), IrExpr::Try { .. })
                        .then_some(initializer);
                    let outer_try = std::mem::replace(&mut self.declared_try, declared_try);
                    let source = self.emit_consumed_operand(initializer, code);
                    self.declared_try = outer_try;
                    self.leave_declared_initializer(previous);
                    source
                } else if self.ir.destructuring_containers.contains(&declaration) {
                    // kotlinc declares the `<destruct>` container as a variable, so a `try`
                    // initializer's result reload returns to the declaration's line.
                    let declared_try = matches!(self.ir.expr(initializer), IrExpr::Try { .. })
                        .then_some(initializer);
                    let outer_try = std::mem::replace(&mut self.declared_try, declared_try);
                    let source = self.emit_consumed_operand(initializer, code);
                    self.declared_try = outer_try;
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
        // An inlined body forgets the caller's line. kotlinc's `visitVariable` marks the
        // declaration before it narrows the erased result, so the `checkcast` — not the store
        // after it — starts that line (`val value = fetch(slot)`). An unsigned
        // bitwise result still forgets after its carrier conversion, and the store is the mark
        // that follows.
        // A same-module inline call's operand is stored into its parameter slot as kotlinc's
        // `genValueAndPut` does, with no line of the declaration's.
        let marks_line = self.copied_operand_binding != Some(declaration);
        let restore_inlined_line = marks_line
            && code.line_forgotten()
            && !self.completes_unsigned_bitwise_value(initializer);
        if restore_inlined_line {
            self.mark_declared_initializer_line(declaration, initializer, code);
        }
        self.adapt_physical_operand(source, semantic, Some(semantic_ty), slot_ty, code);
        if self.completes_unsigned_bitwise_value(initializer) {
            code.forget_line();
        }
        if marks_line && !restore_inlined_line {
            self.mark_declared_initializer_line(declaration, initializer, code);
        }
    }

    /// The declaration's line, after the initializer's own start when that start is a different
    /// expression. A declaration inside an external inline body maps through the class source map.
    fn mark_declared_initializer_line(
        &mut self,
        declaration: ExprId,
        initializer: ExprId,
        code: &mut CodeBuilder,
    ) {
        self.mark_expression_start(initializer, code);
        match self.ir.external_frame_lines.get(&declaration).cloned() {
            Some(frame) => {
                let line = self.map_external_frame_line(&frame);
                code.mark_line(line);
            }
            None => {
                if let Some(&line) = self.ir.expr_lines.get(&declaration) {
                    self.mark_expression_line(declaration, line, code);
                }
            }
        }
    }
}
