//! JVM emission for common-IR lexical blocks and their local-slot lifetimes.

use std::collections::HashMap;

use super::frame_map::{FrameKey, Mark};
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
        self.link_safe_call_chain(block, code);
        let saved = self.open_slot_scope();
        let terminal_target = self.terminal_statement_target.take();
        let marked_initializer = self.renders_initializer_boundary(block);
        self.mark_initializer_line(block, false, code);
        self.emit_open_block(stmts, value, terminal_target, code);
        self.mark_initializer_line(block, true, code);
        if !self.ir.callable_scopes.contains(&block) {
            self.close_scope_locals(code, marked_initializer);
            self.close_spliced_lambda_frame(block, code);
        }
        self.block_depth -= 1;
        self.restore_slot_scope(saved);
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
        self.link_safe_call_chain(block, code);
        let enclosing_statement_line = self.statement_line;
        let saved = self.open_slot_scope();
        self.block_depth += 1;
        let mut dead = false;
        for &statement in stmts {
            self.mark_statement_line(statement, code);
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
                self.emit_value(value, code);
            }
        }
        // A callable's own scope is its body. A value-returning lambda keeps those locals
        // open through the return, the same way a statement-position callable scope does.
        if !self.ir.callable_scopes.contains(&block) {
            self.close_scope_locals(code, false);
        }
        self.close_spliced_lambda_frame(block, code);
        self.block_depth -= 1;
        self.restore_slot_scope(saved);
        self.statement_line = enclosing_statement_line;
    }

    /// Close the frame of a lambda body spliced into an inline call, once its locals are closed.
    ///
    /// kotlinc's inliner returns to the inline body's own line at the invocation the lambda
    /// replaced, with a `nop` to carry it. Its nop cleanup keeps that `nop` only where no other
    /// instruction follows on the line before the next debug point, which is the same decision
    /// the optimizer pipeline makes over this one.
    fn close_spliced_lambda_frame(&mut self, block: u32, code: &mut CodeBuilder) {
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
        if let Some(&line) = self.ir.expr_source_lines.get(&block) {
            self.mark_expression_line(block, line, code);
        }
        code.nop();
    }

    /// Emit one IR block while leaving its lexical slot scope open. The ordinary `Block` arm closes
    /// it immediately; a post-test loop closes it only after emitting the bottom condition, whose
    /// Kotlin scope includes declarations from the body.
    pub(super) fn emit_open_block(
        &mut self,
        stmts: Vec<u32>,
        value: Option<u32>,
        terminal_target: Option<Label>,
        code: &mut CodeBuilder,
    ) {
        let enclosing_statement_line = self.statement_line;
        self.block_depth += 1;
        let mut dead = false;
        let last_statement = stmts.len().checked_sub(1);
        self.note_inlined_only_cells(&stmts, value);
        for (index, statement) in stmts.into_iter().enumerate() {
            self.mark_statement_line(statement, code);
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
        if !dead {
            if let Some(value) = value {
                self.mark_statement_line(value, code);
                self.emit_discarding(value, code);
            }
        }
        self.terminal_statement_target = None;
        // The block's source statements are nested inside the expression its caller is emitting.
        // Once it closes, an instruction consuming the block's value belongs to that enclosing
        // statement, not to the last branch/statement visited inside the block.
        self.statement_line = enclosing_statement_line;
    }

    pub(super) fn mark_statement_line(&mut self, statement: u32, code: &mut CodeBuilder) {
        if let Some(line) = self.ir.expr_lines.get(&statement).copied() {
            self.mark_expression_line(statement, line, code);
        }
        // A line stays in effect until another statement replaces it, exactly as the
        // `LineNumberTable` reads: a statement without a line of its own does not clear it.
        if let Some(line) = self.ir.expr_lines.get(&statement).copied() {
            self.statement_line = Some(line);
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
                let length = end.saturating_sub(local.start);
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
        self.slots = scope.slots;
        self.unassigned_values
            .retain(|value| self.slots.contains_key(value));
        self.frame.leave_block(scope.frame);
    }
}

/// An open lexical slot scope; see [`Emitter::open_slot_scope`].
pub(super) struct SlotScope {
    slots: HashMap<u32, (u16, Ty)>,
    frame: Mark,
}

/// A source local whose debug range is open: declared in the block at `depth`, live in `slot`
/// from `start`.
pub(super) struct OpenLocal {
    pub(super) depth: usize,
    pub(super) slot: u16,
    pub(super) start: u16,
    pub(super) name: String,
    pub(super) descriptor: String,
    /// This local binds an inline call operand and starts with the inline frame, after all operands
    /// have been evaluated, rather than at its individual store.
    pub(super) inline_operand: bool,
    /// Where in the table the entry goes when it must precede entries recorded after it opened,
    /// rather than follow them.
    pub(super) table_position: Option<usize>,
}

impl OpenLocal {
    /// Record the closed range in the method's `LocalVariableTable`.
    pub(super) fn record(self, length: Option<u16>, code: &mut CodeBuilder) {
        let entry = (self.start, length, self.slot, self.name, self.descriptor);
        match self.table_position {
            Some(position) => code.insert_local_entry(position, entry),
            None => code.add_local_entry(entry.0, entry.1, entry.2, &entry.3, &entry.4),
        }
    }
}
