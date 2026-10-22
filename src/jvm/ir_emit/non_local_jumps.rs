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
//! A return of an inline expansion outside the lambda (`return@outer` inside `withLock`, written in
//! `ownRun outer@ { ... }`) is a `break` of the expansion's return frame, a loop of the code outside
//! the lambda, that carries the frame's result. kotlinc returns it from the lambda's method as the
//! value of a marked return naming the frame, and the inliner copies the inlined function's `finally`
//! blocks around it as it does for a return of the function's value.
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
    /// The return frame outside the lambda node that a store of its result has just left for:
    /// the frame's `break` that follows the store has nothing left to do.
    left_with_result: Option<String>,
}

/// How a marked return leaves for a loop outside the lambda node.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopExit {
    Break,
    Continue,
    /// A `break` of an inline expansion's return frame carrying the frame's result.
    FrameResult,
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

const MARKER_PREFIXES: [(&str, LoopExit); 3] = [
    ("break$", LoopExit::Break),
    ("continue$", LoopExit::Continue),
    ("result$", LoopExit::FrameResult),
];

fn marker_name(position: usize, exit: LoopExit) -> String {
    let (prefix, _) = MARKER_PREFIXES
        .iter()
        .find(|(_, candidate)| *candidate == exit)
        .expect("every loop exit has a marker prefix");
    format!("{prefix}{position}")
}

/// The loop a marker name of [`marker_name`] leaves, and how.
fn marked_loop(name: &str) -> Option<(usize, LoopExit)> {
    MARKER_PREFIXES.iter().find_map(|&(prefix, exit)| {
        let position = name.strip_prefix(prefix)?.parse().ok()?;
        Some((position, exit))
    })
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

    /// Leave the lambda node being compiled with the result of the return frame `frame`, which is
    /// on the stack, when the frame is a loop outside the node: the store of the result is the
    /// frame's return, and the frame's `break` that follows it in the checked body adds nothing.
    /// Returns whether it left.
    pub(super) fn leave_lambda_node_with_result(
        &mut self,
        frame: &str,
        code: &mut CodeBuilder,
    ) -> bool {
        if self
            .loop_stack
            .iter()
            .any(|(_, _, label, _)| label.as_deref() == Some(frame))
        {
            return false;
        }
        let Some(position) = self
            .non_local_jumps
            .outer_loops
            .iter()
            .rposition(|label| label.as_deref() == Some(frame))
        else {
            return false;
        };
        self.leave_lambda_node(position, LoopExit::FrameResult, code);
        self.non_local_jumps.left_with_result = Some(frame.to_string());
        true
    }

    /// A `break` or `continue` of the loop at `position` outside the lambda node being compiled:
    /// the node's own `finally` blocks, then its marked `return`. The `break` of a return frame
    /// whose result left the node already ([`Self::leave_lambda_node_with_result`]) adds nothing.
    pub(super) fn emit_non_local_loop_jump(
        &mut self,
        transfer: crate::ir::ExprId,
        position: usize,
        brk: bool,
        code: &mut CodeBuilder,
    ) {
        if let Some(frame) = self.non_local_jumps.left_with_result.take() {
            if brk && self.non_local_jumps.outer_loops[position].as_deref() == Some(frame.as_str())
            {
                return;
            }
            self.run.set_emit_error(
                "a return frame's result is not followed by the frame's break".to_string(),
            );
            return;
        }
        self.mark_expression_start(transfer, code);
        code.nop();
        let exit = if brk {
            LoopExit::Break
        } else {
            LoopExit::Continue
        };
        self.leave_lambda_node(position, exit, code);
    }

    /// The node's own `finally` blocks, then the marked return that leaves for the loop at
    /// `position` outside it.
    fn leave_lambda_node(&mut self, position: usize, exit: LoopExit, code: &mut CodeBuilder) {
        let result = match exit {
            LoopExit::FrameResult => match self.loop_result_type(position) {
                Some(ty) => ty,
                None => {
                    self.run.set_emit_error(
                        "a marked frame result names a loop that is not a return frame".to_string(),
                    );
                    return;
                }
            },
            LoopExit::Break | LoopExit::Continue => Ty::Unit,
        };
        if code.stack_height() != i32::from(slot_words(result)) {
            self.run.set_emit_error(
                "a non-local break or continue leaves operands on the stack".to_string(),
            );
            return;
        }
        if self.emit_finalizers_with_value(result, 0, code) {
            self.non_local_jumps
                .exits
                .push((code.bytes.len(), marker_name(position, exit)));
            emit_return(result, code);
        }
        self.reopen_finally_segments(code);
    }

    /// The type of the result the return frame at `position` carries, counted as
    /// [`LoopTransferTarget::Outer`] counts.
    fn loop_result_type(&self, position: usize) -> Option<Ty> {
        let outer = &self.non_local_jumps.outer_loops;
        let label = match position.checked_sub(outer.len()) {
            None => outer.get(position)?.as_deref()?,
            Some(own) => self.loop_stack.get(own)?.2.as_deref()?,
        };
        self.inline_return_frame_results
            .get(label)
            .map(|&(_, ty)| ty)
    }

    /// The loop a marked return of an inlined node leaves, if it leaves one rather than the
    /// function.
    pub(super) fn inlined_loop_jump(name: &str) -> Option<(usize, LoopExit)> {
        marked_loop(name)
    }

    /// Write a marked return of an inlined node that leaves the loop at `position`. A loop of this
    /// code is left behind the `finally` blocks between here and it (kotlinc's
    /// `generateAndInsertFinallyBlocks` with the loop's label), a return frame's result stored as
    /// the frame's own `break` stores it; a loop further out leaves this lambda node in turn.
    pub(super) fn emit_inlined_loop_jump(
        &mut self,
        position: usize,
        exit: LoopExit,
        code: &mut CodeBuilder,
    ) {
        let outer = self.non_local_jumps.outer_loops.len();
        let Some(own) = position.checked_sub(outer) else {
            self.leave_lambda_node(position, exit, code);
            return;
        };
        let Some((cont, end, label, depth)) = self.loop_stack.get(own).cloned() else {
            self.run.set_emit_error(
                "an inlined break or continue names a loop that is not open here".to_string(),
            );
            return;
        };
        if exit == LoopExit::FrameResult {
            let Some(&(value, ty)) = label
                .as_ref()
                .and_then(|label| self.inline_return_frame_results.get(label))
            else {
                self.run.set_emit_error(
                    "a marked frame result names a loop that is not a return frame".to_string(),
                );
                return;
            };
            let slot = self.activate_inline_return_frame_result(value, ty);
            store(ty, slot, code);
            self.unassigned_values.remove(&value);
        }
        if code.stack_height() != 0 {
            self.run.set_emit_error(
                "a non-local break or continue leaves operands on the stack".to_string(),
            );
            return;
        }
        let brk = exit != LoopExit::Continue;
        if self.emit_transfer_finalizers(depth, code) {
            self.finish_loop_transfer(&label, if brk { end } else { cont }, brk, None, code);
        }
        self.reopen_finally_segments(code);
    }
}
