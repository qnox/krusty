//! `break` and `continue` that leave an inline lambda compiled for the MethodInliner port for a
//! loop of the code the lambda is written into.
//!
//! kotlinc's `visitBreakContinue` treats such a transfer as a non-local return: the lambda's method
//! ends it with a `return` behind a `$$$$$NON_LOCAL_RETURN$$$$$` marker naming the loop
//! (`generateGlobalReturnFlag`), so the inliner copies the inlined function's `finally` blocks in
//! front of it as it does for a return. Where the inlined code is written into its caller, the
//! marked return becomes the caller's jump to the loop, behind the caller's `finally` blocks that
//! the jump leaves.
//!
//! A loop is named by its position in the loops open where the outermost lambda is written,
//! followed by those each nested lambda opens itself: the same position names the same loop in
//! every node a marked return passes through.

use super::*;

/// The loops a lambda node being compiled can leave, and the jumps it leaves them by.
#[derive(Default)]
pub(super) struct NonLocalJumps {
    /// The checked target identity of every loop open outside the lambda node being compiled,
    /// outermost first. Empty while a function's own code is compiled.
    outer_loops: Vec<Option<String>>,
    /// Each jump out of the lambda node being compiled: the offset of its `return` in the node's
    /// code, and the name of the marker that goes in front of it.
    exits: Vec<(usize, String)>,
}

/// What a lambda node compiled with [`Emitter::enter_lambda_loops`] restores on leaving.
pub(super) struct CallerLoops {
    loop_stack: Vec<(Label, Label, Option<String>, usize)>,
    outer_loops: usize,
    exits: Vec<(usize, String)>,
}

/// The loop a `break` or `continue` leaves.
pub(super) enum LoopTransferTarget {
    /// A loop of the code being compiled: its continue target, its exit target, and the finalizer
    /// depth at its entry.
    Own(Label, Label, usize),
    /// A loop outside the lambda node being compiled, by its position.
    Outer(usize),
}

fn marker_name(position: usize, brk: bool) -> String {
    format!("{}{position}", if brk { "break$" } else { "continue$" })
}

/// The loop a marker name of [`marker_name`] leaves, and whether it breaks out of it.
fn marked_loop(name: &str) -> Option<(usize, bool)> {
    if let Some(position) = name.strip_prefix("break$") {
        return position.parse().ok().map(|position| (position, true));
    }
    name.strip_prefix("continue$")?
        .parse()
        .ok()
        .map(|position| (position, false))
}

impl Emitter<'_> {
    /// The loop `label` names: the innermost open loop of the code being compiled that carries
    /// it, else the innermost loop outside the lambda node being compiled that does. An unlabeled
    /// transfer leaves the innermost loop. `None` when no open loop answers.
    pub(super) fn loop_transfer_target(
        &self,
        label: &Option<String>,
    ) -> Option<LoopTransferTarget> {
        let own = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|(_, _, sl, _)| sl.as_deref() == Some(l.as_str())),
            None => self.loop_stack.last(),
        };
        if let Some(&(cont, end, _, depth)) = own {
            return Some(LoopTransferTarget::Own(cont, end, depth));
        }
        let outer = &self.non_local_jumps.outer_loops;
        match label {
            Some(l) => outer
                .iter()
                .rposition(|sl| sl.as_deref() == Some(l.as_str())),
            None => outer.len().checked_sub(1),
        }
        .map(LoopTransferTarget::Outer)
    }

    /// Compile a lambda node's body: the loops open here become loops outside it, and the jumps
    /// out of it start afresh.
    pub(super) fn enter_lambda_loops(&mut self) -> CallerLoops {
        let loop_stack = std::mem::take(&mut self.loop_stack);
        let outer_loops = self.non_local_jumps.outer_loops.len();
        self.non_local_jumps
            .outer_loops
            .extend(loop_stack.iter().map(|(_, _, label, _)| label.clone()));
        CallerLoops {
            loop_stack,
            outer_loops,
            exits: std::mem::take(&mut self.non_local_jumps.exits),
        }
    }

    /// Restore the loops of the code the lambda node is written into; returns the node's jumps
    /// out of it.
    pub(super) fn leave_lambda_loops(&mut self, caller: CallerLoops) -> Vec<(usize, String)> {
        self.loop_stack = caller.loop_stack;
        self.non_local_jumps
            .outer_loops
            .truncate(caller.outer_loops);
        std::mem::replace(&mut self.non_local_jumps.exits, caller.exits)
    }

    /// A `break` or `continue` of the loop at `position` outside the lambda node being compiled:
    /// the node's own `finally` blocks, then its marked `return`.
    pub(super) fn emit_non_local_loop_jump(
        &mut self,
        transfer: crate::ir::ExprId,
        position: usize,
        brk: bool,
        code: &mut CodeBuilder,
    ) {
        self.mark_expression_start(transfer, code);
        code.nop();
        if self.emit_transfer_finalizers(0, code) {
            self.leave_lambda_node(position, brk, code);
        }
        self.reopen_finally_segments(code);
    }

    fn leave_lambda_node(&mut self, position: usize, brk: bool, code: &mut CodeBuilder) {
        if code.stack_height() != 0 {
            self.run.set_emit_error(
                "a non-local break or continue leaves operands on the stack".to_string(),
            );
            return;
        }
        self.non_local_jumps
            .exits
            .push((code.bytes.len(), marker_name(position, brk)));
        code.ret_void();
    }

    /// The loop a marked return of an inlined node leaves, if it leaves one rather than the
    /// function.
    pub(super) fn inlined_loop_jump(name: &str) -> Option<(usize, bool)> {
        marked_loop(name)
    }

    /// Write a marked return of an inlined node that leaves the loop at `position`. A loop of this
    /// code is left behind the `finally` blocks between here and it (kotlinc's
    /// `generateAndInsertFinallyBlocks` with the loop's label); a loop further out leaves this
    /// lambda node in turn.
    pub(super) fn emit_inlined_loop_jump(
        &mut self,
        position: usize,
        brk: bool,
        code: &mut CodeBuilder,
    ) {
        let outer = self.non_local_jumps.outer_loops.len();
        let Some(own) = position.checked_sub(outer) else {
            if self.emit_transfer_finalizers(0, code) {
                self.leave_lambda_node(position, brk, code);
            }
            self.reopen_finally_segments(code);
            return;
        };
        let Some((cont, end, label, depth)) = self.loop_stack.get(own).cloned() else {
            self.run.set_emit_error(
                "an inlined break or continue names a loop that is not open here".to_string(),
            );
            return;
        };
        if code.stack_height() != 0 {
            self.run.set_emit_error(
                "a non-local break or continue leaves operands on the stack".to_string(),
            );
            return;
        }
        if self.emit_transfer_finalizers(depth, code) {
            self.finish_loop_transfer(&label, if brk { end } else { cont }, brk, None, code);
        }
        self.reopen_finally_segments(code);
    }
}
