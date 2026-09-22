//! JVM emission for common-IR lexical blocks and their local-slot lifetimes.

use std::collections::HashMap;

use super::{debug_lines, CodeBuilder, Emitter, Label, Ty};

impl Emitter<'_> {
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
        debug_lines::mark_statement(self.ir, statement, code);
        // A line stays in effect until another statement replaces it, exactly as the
        // `LineNumberTable` reads: a statement without a line of its own does not clear it.
        if let Some(line) = self.ir.expr_lines.get(&statement).copied() {
            self.statement_line = Some(line);
        }
    }

    /// Close source-local debug ranges declared in the current nested block.
    pub(super) fn close_scope_locals(&mut self, code: &mut CodeBuilder) {
        if self.block_depth <= 1 {
            return;
        }
        let end = code.bytes.len().min(u16::MAX as usize) as u16;
        let depth = self.block_depth;
        let mut i = 0;
        while i < self.open_locals.len() {
            if self.open_locals[i].0 >= depth {
                let (_, slot, start, name, desc) = self.open_locals.remove(i);
                self.cw.seed_utf8(&name);
                self.cw.seed_utf8(&desc);
                code.add_local_entry(start, Some(end.saturating_sub(start)), slot, &name, &desc);
            } else {
                i += 1;
            }
        }
    }

    /// Restore the lexical value map while retaining the backend's monotonic physical slot cursor.
    pub(super) fn restore_slot_scope(&mut self, slots: HashMap<u32, (u16, Ty)>) {
        self.slots = slots;
        self.unassigned_values
            .retain(|value| self.slots.contains_key(value));
    }
}
