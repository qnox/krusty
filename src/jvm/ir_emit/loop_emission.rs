//! JVM emission of a common-IR loop (kotlinc's `visitWhileLoop` and `visitDoWhileLoop`).

use crate::ir::{ExprId, IrExpr};

use super::{debug_lines, CodeBuilder, Emitter};

impl Emitter<'_> {
    /// Emit an `IrExpr::While`: a pre-test loop, a `do…while`, or either with an update run at the
    /// `continue` target.
    ///
    /// Lowering generates a `for` loop's control — its update, its exit test, the bottom condition of
    /// its `do…while` shape — out of the loop itself, as kotlinc's `ForLoopsLowering` builds them at
    /// the loop's offsets, so that control carries the loop's source line. A `for` is always a
    /// statement, so its line is the statement line in effect where the loop begins.
    pub(super) fn emit_while(&mut self, expression: ExprId, code: &mut CodeBuilder) {
        let IrExpr::While {
            cond,
            body,
            update,
            post_test,
            label,
        } = self.ir.expr(expression).clone()
        else {
            unreachable!("emit_while is called on a loop");
        };
        let loop_line = self.statement_line;
        let start = code.new_label();
        let cont = code.new_label();
        let end = code.new_label();
        self.bind(start, code);
        // A pre-test loop checks the condition before the body; a `do…while` skips this and
        // tests at the bottom (`cont`), so the body always runs once.
        if !post_test && self.emit_cond_branch(cond, end, false, code) {
            // `while (false)`: the jump-out is unconditional, so the body/update/back-edge
            // that would follow are unreachable — emitting them leaves frameless dead code
            // the verifier rejects. kotlinc emits no body for a never-entered loop either.
            self.bind(end, code);
            return;
        }
        // `continue` targets `cont` (run the update / bottom test); `break` targets `end`.
        //
        // A PRE-TEST loop with no update has nothing at the bottom but the back edge, so
        // `cont` would be a jump to a jump: `continue` reaches the condition either way, and
        // kotlinc branches to it directly. Using the condition itself as the continue target
        // removes the hop, and leaves the back edge below reachable only by falling out of
        // the body — where a body that always jumps makes it dead, which is what kotlinc
        // emits for a loop whose every path continues or breaks.
        let bottom = if post_test || update.is_some() {
            cont
        } else {
            start
        };
        self.loop_stack
            .push((bottom, end, label.clone(), self.return_finalizers.len()));
        let enclosing_terminal_target = self.terminal_statement_target.replace(bottom);
        // A lowered `for` loop's body is a transparent scope (kotlinc's `IrComposite`): what it
        // declares itself stays in scope until the loop ends.
        let transparent_body = !post_test && self.ir.transparent_loop_bodies.contains(&expression);
        let mut retained_body_scope = if post_test || transparent_body {
            match self.ir.expr(body).clone() {
                // Kotlin's `do` body and bottom condition share one lexical scope. Keep the
                // body's exact local-slot map alive until after the condition; closing the
                // ordinary block here would make a legal `do { val x = ... } while (x ...)`
                // read an undeclared value. `post_test` is the checked common-IR fact that
                // authorizes this lifetime, so no source-shape lookup is involved.
                IrExpr::Block { stmts, value } => {
                    let saved = self.open_slot_scope();
                    let discard_after_close =
                        self.emit_open_block(stmts, value, Some(bottom), code);
                    if let Some(discarded) = discard_after_close {
                        super::discard(self.value_ty(discarded), code);
                    }
                    Some(saved)
                }
                _ => {
                    self.emit(body, code);
                    None
                }
            }
        } else {
            self.emit(body, code);
            None
        };
        self.terminal_statement_target = enclosing_terminal_target;
        // A pre-test body's block has restored its slot map. A post-test body's block stays
        // open here because `continue` and the condition are inside that same Kotlin scope.
        if bottom == cont {
            self.bind(cont, code);
        }
        if post_test
            && retained_body_scope.is_some()
            && !self.ir.for_loop_next_loops.contains(&expression)
        {
            self.end_unreferenced_do_while_locals([update, Some(cond)], code);
        }
        // The update is part of the loop, so it keeps the `break`/`continue` scope active — the
        // non-overflowing counted loop puts its `if (i == end) break` here (before the increment)
        // so a `continue` lands on it too, instead of skipping straight to the wrapping `i++`.
        if let Some(u) = update {
            debug_lines::mark_loop_control(loop_line, code);
            self.emit(u, code);
        }
        if post_test {
            // `do…while`: loop back while the condition holds, then fall through to `end`.
            // A `while (true)` back-edge IS unconditional, and the only thing after it is the
            // `frame(end)`/`bind(end)` below — which is exactly what a dead-but-framed `end`
            // needs, so the flag is deliberately ignored here. Anything emitted after this
            // point in future would have to honour it.
            //
            // The condition is inside this loop: `do { … } while (break)` leaves THIS loop.
            // Popping the loop first would make that transfer miss the loop it names.
            if !self.ir.expr_source_lines.contains_key(&cond) {
                debug_lines::mark_loop_control(loop_line, code);
            }
            let _ = self.emit_cond_branch(cond, start, true, code);
            if let Some(saved) = retained_body_scope.take() {
                self.close_scope_locals(code, false);
                self.block_depth -= 1;
                self.restore_slot_scope(saved);
            }
        } else {
            code.goto(start);
        }
        self.loop_stack.pop();
        self.bind(end, code);
        if let Some(saved) = retained_body_scope {
            self.close_scope_locals(code, false);
            self.block_depth -= 1;
            self.restore_slot_scope(saved);
        }
    }

    /// `endUnreferencedDoWhileLocals`: a local the `do…while` body declares may be undeclared when
    /// a `continue` reaches the condition, so unless the condition reads it, its range ends where
    /// the condition starts. The update is part of kotlinc's condition.
    fn end_unreferenced_do_while_locals(
        &mut self,
        condition: [Option<ExprId>; 2],
        code: &mut CodeBuilder,
    ) {
        let mut read = std::collections::HashSet::new();
        let mut pending: Vec<ExprId> = condition.into_iter().flatten().collect();
        while let Some(expression) = pending.pop() {
            if let IrExpr::GetValue(value) = self.ir.expr(expression) {
                if let Some(&(slot, _)) = self.slots.get(value) {
                    read.insert(slot);
                }
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }
        let end = code.bytes.len().min(u16::MAX as usize) as u16;
        let depth = self.block_depth;
        for local in &mut self.open_locals {
            if local.depth == depth && !read.contains(&local.slot) {
                local.explicit_end.get_or_insert(end);
            }
        }
    }
}
