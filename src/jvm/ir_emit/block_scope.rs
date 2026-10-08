//! JVM emission for common-IR lexical blocks and their local-slot lifetimes.

use std::collections::HashMap;

use super::frame_map::{FrameKey, Mark, TempRole, TempSlot};
use super::{CodeBuilder, Emitter, Label, Ty};
use crate::ir::{IrDebugLocalProvenance, IrExpr};

impl Emitter<'_> {
    /// Emit a block in statement position within its own lexical slot scope. Restoring the slot
    /// *map* afterwards keeps a local declared here out of a later merge-point frame: its slot must
    /// read as `Top` once out of scope, or a sibling branch that never initialized it fails
    /// verification. A callable's own scope leaves its debug ranges open to the callable's end, so
    /// its locals cover the return.
    pub(super) fn emit_statement_block(
        &mut self,
        block: u32,
        stmts: Vec<u32>,
        value: Option<u32>,
        code: &mut CodeBuilder,
    ) {
        // A `suspendCoroutineUninterceptedOrReturn` block statement is still a suspension point:
        // its result is probed and discarded the way any suspension statement's is.
        if self.is_transformed_block(block) {
            self.emit_discarding(block, code);
            return;
        }
        let inline_stack_height = code.stack_height();
        let normalizes_inline_stack =
            self.ir.external_inline_expansions.contains(&block) && inline_stack_height != 0;
        // A spliced lambda's marker follows evaluation of the inline call's ordinary operands.
        // Preserve the caller stack there so argument temporaries keep kotlinc's slot order. An
        // expansion without a lambda marker (for example an iteration plan) still starts at the
        // block boundary.
        let delays_stack_normalization = normalizes_inline_stack
            && stmts.iter().any(|&statement| {
                matches!(
                    self.ir.debug_local_provenance(statement),
                    Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                )
            });
        if normalizes_inline_stack && !delays_stack_normalization {
            code.inline_call_marker(true);
        }
        self.link_safe_call_chain(block, code);
        let enclosing_statement_line = self.statement_line;
        let saved = self.open_slot_scope();
        let terminal_target = self.terminal_statement_target.take();
        let marked_initializer = self.renders_initializer_boundary(block);
        self.mark_initializer_line(block, false, code);
        // The optimized safe call still owns an ordinary lexical block. Enter its depth before
        // asking for the fast path so nested selector emission, local ranges, and frames observe
        // the same lifecycle as `emit_open_block`.
        self.block_depth += 1;
        let EmittedBlock {
            discard_after_close,
            reserved_inline_stack,
        } = if self.try_emit_duplicated_safe_call(&stmts, value, true, code) {
            self.terminal_statement_target = None;
            self.statement_line = enclosing_statement_line;
            EmittedBlock {
                discard_after_close: None,
                reserved_inline_stack: None,
            }
        } else {
            self.block_depth -= 1;
            self.emit_open_block(
                stmts,
                value,
                terminal_target,
                delays_stack_normalization,
                inline_stack_height,
                code,
            )
        };
        self.mark_initializer_line(block, true, code);
        if !self.ir.callable_scopes.contains(&block) {
            self.close_external_inline_frame(block, code);
            self.close_scope_locals(code, marked_initializer);
        }
        if normalizes_inline_stack
            && (!delays_stack_normalization || reserved_inline_stack.is_some())
        {
            code.inline_call_marker(false);
        }
        // The lambda locals end at this `nop`. It returns to the inline body's line, and that
        // local-end label is what keeps it when the following instruction is the next debug point.
        if !self.ir.callable_scopes.contains(&block) {
            self.close_spliced_lambda_frame(block, false, code);
        }
        if let Some(discarded) = discard_after_close {
            super::discard(self.value_ty(discarded), code);
        }
        self.block_depth -= 1;
        self.restore_slot_scope(saved);
        self.release_reserved_inline_stack(reserved_inline_stack);
    }

    /// Emit a block in value position: its statements run for effect and its trailing value is
    /// left on the stack. Its locals are scoped (the slot map restored) so they do not leak into an
    /// outer frame.
    pub(super) fn emit_value_block(
        &mut self,
        block: u32,
        stmts: &[u32],
        value: Option<u32>,
        code: &mut CodeBuilder,
    ) {
        // External inline plans record only their semantic boundary in common IR. When evaluation
        // begins with caller operands already live, the finished-method FixStack pass derives and
        // restores their complete JVM representation from these markers.
        let inline_stack_height = code.stack_height();
        let normalizes_inline_stack =
            self.ir.external_inline_expansions.contains(&block) && inline_stack_height != 0;
        let delays_stack_normalization = normalizes_inline_stack
            && stmts.iter().any(|&statement| {
                matches!(
                    self.ir.debug_local_provenance(statement),
                    Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                )
            });
        if normalizes_inline_stack && !delays_stack_normalization {
            code.inline_call_marker(true);
        }
        self.link_safe_call_chain(block, code);
        let enclosing_statement_line = self.statement_line;
        let saved = self.open_slot_scope();
        self.block_depth += 1;
        let mut dead = false;
        let mut reserved_inline_stack = None;
        if !self.try_emit_duplicated_safe_call(stmts, value, false, code) {
            let mut normalize_at_lambda_frame = delays_stack_normalization;
            for &statement in stmts {
                // The lambda-frame line owns any caller-stack stores that FixStack inserts at the
                // opening marker. Emit the line first so those stores, not only the marker local
                // that follows them, start the mapped inline interval.
                self.mark_statement_line(statement, code);
                if normalize_at_lambda_frame
                    && matches!(
                        self.ir.debug_local_provenance(statement),
                        Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                    )
                {
                    reserved_inline_stack =
                        Some(self.open_reserved_inline_stack(inline_stack_height, code));
                    normalize_at_lambda_frame = false;
                }
                // A statement nets zero on the operand stack (its value is stored/discarded). Reset the
                // tracked height to that baseline afterward: raw spliced control flow is opaque to the
                // builder's linear counter and can leave `cur_stack` drifted above the real,
                // verified-balanced height. Later emission still relies on accurate physical stack
                // accounting even though final-body analysis owns verifier frames.
                let base = code.stack_height();
                self.emit(statement, code);
                if self.discarding_diverges(statement) {
                    dead = true;
                    break;
                }
                code.set_stack(base.max(0) as u16);
            }
            if !dead {
                if let Some(value) = value {
                    self.mark_statement_line(value, code);
                    // `{ effects; }` coerced to `Unit` is `Block { effects, value: UnitInstance }`.
                    // The lambda `}` stands between those effects and `Unit.INSTANCE`.
                    if self.opens_spliced_lambda_frame(stmts) {
                        if let Some((effects, unit)) = self.unit_after_lambda_effects(value) {
                            for effect in effects {
                                let base = code.stack_height();
                                self.emit(effect, code);
                                code.set_stack(base.max(0) as u16);
                            }
                            self.close_lambda_value_frame(block, code);
                            self.emit_value(unit, code);
                            if normalizes_inline_stack
                                && (!delays_stack_normalization || reserved_inline_stack.is_some())
                            {
                                code.inline_call_marker(false);
                            }
                            self.block_depth -= 1;
                            self.restore_slot_scope(saved);
                            self.release_reserved_inline_stack(reserved_inline_stack);
                            self.statement_line = enclosing_statement_line;
                            return;
                        }
                    }
                    self.emit_value(value, code);
                }
            }
        }
        // A callable's own scope is its body. A value-returning lambda keeps those locals
        // open through the return, the same way a statement-position callable scope does.
        if !self.ir.callable_scopes.contains(&block) {
            self.close_external_inline_frame(block, code);
            self.close_scope_locals(code, false);
        }
        if normalizes_inline_stack
            && (!delays_stack_normalization || reserved_inline_stack.is_some())
        {
            code.inline_call_marker(false);
        }
        // After the lambda locals, including when this block is itself a callable scope and those
        // locals stay open through the return. The `nop` is the inline body's line, not the brace.
        self.close_spliced_lambda_frame(block, false, code);
        self.block_depth -= 1;
        self.restore_slot_scope(saved);
        self.release_reserved_inline_stack(reserved_inline_stack);
        self.statement_line = enclosing_statement_line;
    }

    fn opens_spliced_lambda_frame(&self, stmts: &[u32]) -> bool {
        stmts.iter().any(|&statement| {
            matches!(
                self.ir.debug_local_provenance(statement),
                Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
            )
        })
    }

    /// Effects of a `Unit` coercion, then the `Unit` value that follows the lambda's closing brace.
    fn unit_after_lambda_effects(&self, value: u32) -> Option<(Vec<u32>, u32)> {
        let IrExpr::Block {
            stmts,
            value: Some(unit),
        } = self.ir.expr(value).clone()
        else {
            return None;
        };
        if !matches!(self.ir.expr(unit), IrExpr::UnitInstance)
            || self.opens_spliced_lambda_frame(&stmts)
        {
            return None;
        }
        Some((stmts, unit))
    }

    fn close_lambda_value_frame(&mut self, block: u32, code: &mut CodeBuilder) {
        if !self.ir.callable_scopes.contains(&block) {
            self.close_external_inline_frame(block, code);
            // `{ effects; }` coerced to `Unit`: the `}` is a `nop` while the lambda locals are
            // still open, and `Unit.INSTANCE` follows them.
            self.close_spliced_lambda_frame(block, true, code);
            self.close_scope_locals(code, false);
        } else {
            self.close_spliced_lambda_frame(block, true, code);
        }
    }

    /// Close the frame of an external inline declaration whose body lines map through the class's
    /// source map: the frame's closing line on a `nop`, the way kotlinc's inliner ends the inline
    /// interval. It runs before the frame's locals close so their ranges extend past the `nop`,
    /// and after the expansion's last instruction so a loop exiting the frame lands on it.
    fn close_external_inline_frame(&mut self, block: u32, code: &mut CodeBuilder) {
        let Some(frame) = self.ir.external_frame_closes.get(&block).cloned() else {
            return;
        };
        if code.is_dead() {
            return;
        }
        // The expansion ran under the dependency's own lines: reset the line in effect so the
        // closing mark is written even where it repeats the last one, as after an inlined body.
        code.forget_line();
        let line = self.map_external_frame_line(&frame);
        code.mark_line(line);
        code.nop();
    }

    /// Close the frame of a lambda body spliced into an inline call.
    ///
    /// `brace` is the `{ effects; }` coercion to `Unit`: kotlinc marks the lambda literal's `}`
    /// and emits a `nop` while the lambda locals are still open, then materializes `Unit`. A brace
    /// on its own line starts a new entry; the local-end label on the following `Unit` keeps the
    /// `nop`. A one-line brace shares the body's line and the `nop` goes.
    ///
    /// Otherwise the locals have already ended. The `nop` returns to the inline body's own line
    /// at the invocation the lambda replaced. The local-end label sits on the `nop`, and the next
    /// debug point (the inline-function frame's end, or the consumer) keeps it.
    fn close_spliced_lambda_frame(&mut self, block: u32, brace: bool, code: &mut CodeBuilder) {
        let IrExpr::Block { stmts, .. } = self.ir.expr(block) else {
            return;
        };
        let opens_frame = stmts.iter().any(|&statement| {
            matches!(
                self.ir.debug_local_provenance(statement),
                Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
            )
        });
        if !opens_frame || code.is_dead() {
            return;
        }
        if brace {
            if let Some(line) = self
                .ir
                .expr_end_lines
                .get(&block)
                .copied()
                .filter(|line| *line != 0)
            {
                code.mark_line(line);
            } else if let Some(line) = self.ir.expr_source_lines.get(&block).copied() {
                code.forget_line();
                self.mark_expression_line(block, line, code);
            }
        } else if let Some(&line) = self.ir.expr_source_lines.get(&block) {
            // The spliced body ran under its own lines: kotlinc resets the line in effect after an
            // inlined body, so the frame's closing mark is written even when it names that line.
            code.forget_line();
            self.mark_expression_line(block, line, code);
        }
        code.nop();
    }

    /// Emit one IR block while leaving its lexical slot scope open. The ordinary `Block` arm closes
    /// it immediately; a post-test loop closes it only after emitting the bottom condition, whose
    /// Kotlin scope includes declarations from the body.
    ///
    /// When the block opens a spliced lambda frame and its trailing value is a plain local read
    /// (an inlined callee's `return this`), the read is left on the stack and its expression is
    /// returned instead of being discarded here: the caller pops it once the frame's closing `nop`
    /// stands between the read and the `pop`. kotlinc's inliner emits that read mechanically and
    /// the padding it puts around the inline interval keeps the pair apart until after temporary
    /// elimination — the second read is what keeps the callee's receiver local alive — and the
    /// pop-backward step removes both.
    pub(super) fn emit_open_block(
        &mut self,
        stmts: Vec<u32>,
        value: Option<u32>,
        terminal_target: Option<Label>,
        normalize_at_lambda_frame: bool,
        inline_stack_height: i32,
        code: &mut CodeBuilder,
    ) -> EmittedBlock {
        let enclosing_statement_line = self.statement_line;
        self.block_depth += 1;
        let mut dead = false;
        let last_statement = stmts.len().checked_sub(1);
        let opens_lambda_frame = value.is_some_and(|value| {
            matches!(self.ir.expr(value), IrExpr::GetValue(_))
                && stmts.iter().any(|&statement| {
                    matches!(
                        self.ir.debug_local_provenance(statement),
                        Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                    )
                })
        });
        let mut normalize_at_lambda_frame = normalize_at_lambda_frame;
        let mut reserved_inline_stack = None;
        // The trailing read is thrown away. Record it before the statements, so a break inside
        // them can see that the landing label will not consume the result.
        let discarded_result = (!opens_lambda_frame)
            .then_some(value)
            .flatten()
            .and_then(|value| match self.ir.expr(value) {
                IrExpr::GetValue(index) => Some(*index),
                _ => None,
            });
        if let Some(index) = discarded_result {
            self.discarded_inline_results.insert(index);
        }
        for (index, statement) in stmts.into_iter().enumerate() {
            // FixStack replaces the opening marker with the saved caller stack. Keep that
            // generated store inside the lambda frame's mapped line interval.
            self.mark_statement_line(statement, code);
            if normalize_at_lambda_frame
                && matches!(
                    self.ir.debug_local_provenance(statement),
                    Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                )
            {
                reserved_inline_stack =
                    Some(self.open_reserved_inline_stack(inline_stack_height, code));
                normalize_at_lambda_frame = false;
            }
            let base = code.stack_height();
            self.terminal_statement_target = (value.is_none() && Some(index) == last_statement)
                .then_some(terminal_target)
                .flatten();
            self.emit(statement, code);
            if self.discarding_diverges(statement) {
                dead = true;
                break;
            }
            code.set_stack(base.max(0) as u16);
        }
        let mut discard_after_frame_close = None;
        if !dead {
            if let Some(value) = value {
                self.mark_statement_line(value, code);
                if opens_lambda_frame {
                    self.emit_value(value, code);
                    discard_after_frame_close = Some(value);
                } else {
                    self.emit_discarding(value, code);
                }
            }
        }
        self.terminal_statement_target = None;
        if let Some(index) = discarded_result {
            self.discarded_inline_results.remove(&index);
        }
        // The block's source statements are nested inside the expression its caller is emitting.
        // Once it closes, an instruction consuming the block's value belongs to that enclosing
        // statement, not to the last branch/statement visited inside the block.
        self.statement_line = enclosing_statement_line;
        EmittedBlock {
            discard_after_close: discard_after_frame_close,
            reserved_inline_stack,
        }
    }

    /// Claim the words FixStack will use before the inline lambda's own frame marker claims its
    /// slot. `inline_stack_height` is captured at the expansion boundary: declarations before the
    /// delayed marker are stack-neutral, while their nested emission may make the builder's linear
    /// counter drift from that verified boundary. The emitted marker preserves the allocation
    /// boundary until the finished method is normalized; the reservations themselves keep
    /// subsequent frame-map entries above it.
    fn open_reserved_inline_stack(
        &mut self,
        inline_stack_height: i32,
        code: &mut CodeBuilder,
    ) -> Vec<TempSlot> {
        let Ok(words) = u16::try_from(inline_stack_height) else {
            self.run.set_emit_error(
                "the operand stack at an inline lambda boundary exceeds the JVM limit".to_string(),
            );
            code.inline_call_marker_with_reserved_locals();
            return Vec::new();
        };
        let mut reserved = Vec::with_capacity(usize::from(words));
        for _ in 0..words {
            reserved.push(self.frame.enter_temp(TempRole::InlineStackSpill, Ty::Int));
        }
        code.inline_call_marker_with_reserved_locals();
        reserved
    }

    fn release_reserved_inline_stack(&mut self, reserved: Option<Vec<TempSlot>>) {
        for slot in reserved.into_iter().flatten().rev() {
            self.frame.leave_temp(slot);
        }
    }

    pub(super) fn mark_statement_line(&mut self, statement: u32, code: &mut CodeBuilder) {
        let Some(&line) = self.ir.expr_lines.get(&statement) else {
            return;
        };
        // A dispatch owns the line entry at the physical call, so this writes no second entry
        // and the statement keeps its source line. Otherwise the marked line is the mapped
        // output line of an inline copy: a later instruction that returns to this statement
        // must not write the callee's raw line.
        let marked = if self.ir.dispatch_line(statement).is_none() {
            self.mark_expression_line(statement, line, code)
        } else {
            line
        };
        if line != 0 {
            self.statement_line = Some(marked);
        }
    }

    /// Close source-local debug ranges declared in the current nested block.
    ///
    /// A callable body stays open through its return, so depth 1 is left alone. An `init` block
    /// is itself that depth when it is the constructor's initializer, and its locals still end at
    /// the block: `force` closes them there.
    pub(super) fn close_scope_locals(&mut self, code: &mut CodeBuilder, force: bool) {
        if !force && self.block_depth <= 1 {
            return;
        }
        let end = code.bytes.len().min(u16::MAX as usize) as u16;
        let depth = self.block_depth;
        let mut i = 0;
        while i < self.open_locals.len() {
            if self.open_locals[i].depth >= depth {
                let local = self.open_locals.remove(i);
                let length = local
                    .explicit_end
                    .unwrap_or(end)
                    .saturating_sub(local.start);
                local.record(Some(length), code);
            } else {
                i += 1;
            }
        }
    }

    /// Enter a declared value's slot, keyed as a call operand's holder when `holds_operand`. It is
    /// in scope from here, but unassigned until its store: frames recorded before then read the
    /// slot as `top`.
    pub(super) fn enter_unassigned_value(
        &mut self,
        index: u32,
        ty: Ty,
        holds_operand: bool,
    ) -> u16 {
        let key = if holds_operand {
            FrameKey::CallOperand(index)
        } else {
            FrameKey::Value(index)
        };
        let slot = self.frame.enter(key, ty);
        self.slots.insert(index, (slot, ty));
        self.unassigned_values.insert(index);
        slot
    }

    /// Open a lexical slot scope: the value map to restore and the frame point to leave back to.
    pub(super) fn open_slot_scope(&self) -> SlotScope {
        SlotScope {
            slots: self.slots.clone(),
            frame: self.frame.mark(),
        }
    }

    /// Close a lexical slot scope: restore the value map and leave the locals declared in it, as
    /// kotlinc's block end does.
    pub(super) fn restore_slot_scope(&mut self, scope: SlotScope) {
        // Activation of an outer inline-return result happens inside this scope, after the
        // snapshot. Restoring the snapshot would point the result back at the slot it lent to
        // a nested local (`return@run i * 10` would then read `i`).
        let activated = std::mem::take(&mut self.slots);
        self.slots = scope.slots;
        for (value, binding) in activated {
            if self.slots.contains_key(&value)
                && self.inline_return_frame_result_type(value).is_some()
            {
                self.slots.insert(value, binding);
            }
        }
        self.unassigned_values
            .retain(|value| self.slots.contains_key(value));
        self.frame.leave_block(scope.frame);
    }

    /// The innermost inline frame currently owning emitted debug locals.
    pub(super) fn active_inline_frame_identity(&self) -> Option<u64> {
        self.open_locals
            .iter()
            .filter(|local| local.opens_inline_frame)
            .max_by_key(|local| local.start)
            .and_then(|local| local.inline_frame)
    }
}

/// An open lexical slot scope; see [`Emitter::open_slot_scope`].
pub(super) struct SlotScope {
    slots: HashMap<u32, (u16, Ty)>,
    frame: Mark,
}

pub(super) struct EmittedBlock {
    pub(super) discard_after_close: Option<u32>,
    pub(super) reserved_inline_stack: Option<Vec<TempSlot>>,
}

/// A source local whose debug range is open: declared in the block at `depth`, live in `slot`
/// from `start`.
pub(super) struct OpenLocal {
    pub(super) depth: usize,
    pub(super) slot: u16,
    pub(super) start: u16,
    pub(super) name: String,
    pub(super) descriptor: String,
    /// This local binds an inline call operand and starts with the next emitted inline frame,
    /// after all operands have been evaluated, rather than at its own store.
    pub(super) inline_operand: bool,
    /// Identity of the inline frame this entry belongs to. This is emission provenance, not a JVM
    /// spelling: locals opened before a frame marker are rebound when that marker is emitted.
    pub(super) inline_frame: Option<u64>,
    /// Whether this entry is the marker that opens `inline_frame`.
    pub(super) opens_inline_frame: bool,
    /// A lambda marker is listed ahead of locals its body already recorded. A function marker
    /// follows locals of its frame whose ranges ended before the marker. A local that ends with
    /// the marker stays after `$i$f$`, and operands still open are recorded after it.
    pub(super) precedes_recorded_locals: bool,
    /// Where the range ends when that is before its block does: a `do…while` body's local that the
    /// condition does not read ends where the condition starts.
    pub(super) explicit_end: Option<u16>,
}

impl OpenLocal {
    /// Record the closed range in the method's `LocalVariableTable` with its inline-frame
    /// ownership, so marker order does not depend on lexical scope-close timing.
    pub(super) fn record(self, length: Option<u16>, code: &mut CodeBuilder) {
        let entry = (self.start, length, self.slot, self.name, self.descriptor);
        if self.opens_inline_frame {
            let frame = self
                .inline_frame
                .expect("an inline frame marker has a frame identity");
            code.insert_inline_frame_marker(frame, self.precedes_recorded_locals, entry);
        } else if let Some(frame) = self.inline_frame {
            code.add_inline_frame_local(frame, entry);
        } else {
            code.add_local_entry(entry.0, entry.1, entry.2, &entry.3, &entry.4);
        }
    }
}
