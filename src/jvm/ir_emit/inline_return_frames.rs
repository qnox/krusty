//! Emission state of an inline-return frame.
//!
//! An inline expansion leaves through `break` to its frame's label after storing the returned value
//! as the frame's result. A lambda body spliced inside the frame numbers its values separately, so a
//! return in it names the frame ([`IrExpr::SetFrameResult`]) rather than the result's value index.
//! The frame's result declaration records its value identity under the frame's label when it is
//! emitted; the first store activates that value's deferred JVM slot from whichever value domain
//! it runs in.
//!
//! A `Unit` frame has no result slot. Its labeled return is still a `break` of that frame, and the
//! break uses the frame's own loop exit. That exit is bound after the lambda's closing-brace `nop`.
//! Only the frame's synthetic trailing `break` keeps the fallthrough that enters the `nop`. A nested
//! lambda does not lend its exit to an outer return.

use super::*;

/// Results and closing-brace joins of the inline-return frames in the method being emitted.
#[derive(Default)]
pub(super) struct InlineReturnState {
    /// Deferred result `(value, type)` of each value-producing frame, by the frame's label.
    results: HashMap<String, (u32, Ty)>,
    /// Lambdas whose `Unit` frames defer their loop exits until after the closing brace, innermost
    /// last. Each names the frames it owns.
    braces: Vec<UnitLambdaBrace>,
    /// Trailing synthetic breaks of `Unit` frames whose bodies are being emitted, innermost last.
    /// The label is the fallthrough into that frame's closing brace, when the loop exit itself is
    /// bound after the brace.
    trailing: Vec<TrailingUnitReturn>,
}

struct UnitLambdaBrace {
    frames: Vec<String>,
    /// Loop exits of `frames`, filled in when each frame loop is emitted and bound after the brace.
    ends: Vec<Label>,
    /// The loop exit is the instruction after this lambda method's own return. The return is what
    /// the inliner turns into the closing-brace `nop`, so the exit cannot be bound in the body.
    after_return: bool,
}

struct TrailingUnitReturn {
    frame: String,
    trailing: ExprId,
    fallthrough: Option<Label>,
}

impl InlineReturnState {
    fn insert_result(&mut self, frame: String, value: u32, ty: Ty) {
        self.results.insert(frame, (value, ty));
    }

    fn result(&self, frame: &str) -> Option<(u32, Ty)> {
        self.results.get(frame).copied()
    }

    fn result_type(&self, value: u32) -> Option<Ty> {
        self.results
            .values()
            .find_map(|&(candidate, ty)| (candidate == value).then_some(ty))
    }

    fn result_values(&self) -> impl Iterator<Item = u32> + '_ {
        self.results.values().map(|&(value, _)| value)
    }

    fn push_brace(&mut self, frames: Vec<String>, after_return: bool) {
        self.braces.push(UnitLambdaBrace {
            frames,
            ends: Vec::new(),
            after_return,
        });
    }

    /// Remember `end` as a loop exit bound with the lambda that owns `frame`.
    fn note_end(&mut self, frame: &str, end: Label) -> bool {
        let Some(brace) = self
            .braces
            .iter_mut()
            .rev()
            .find(|brace| brace.frames.iter().any(|candidate| candidate == frame))
        else {
            return false;
        };
        brace.ends.push(end);
        true
    }

    fn brace_len(&self) -> usize {
        self.braces.len()
    }

    fn last_after_return(&self) -> bool {
        self.braces.last().is_some_and(|brace| brace.after_return)
    }

    fn last_return_ends(&self) -> Option<&[Label]> {
        self.braces
            .last()
            .filter(|brace| brace.after_return)
            .map(|brace| brace.ends.as_slice())
    }

    fn pop_brace(&mut self) -> Vec<Label> {
        self.braces
            .pop()
            .expect("a unit lambda brace was pushed")
            .ends
    }

    fn owns_frame(&self, frame: &str) -> bool {
        self.braces
            .iter()
            .rev()
            .any(|brace| brace.frames.iter().any(|candidate| candidate == frame))
    }

    fn enter_trailing(&mut self, frame: String, trailing: ExprId, fallthrough: Option<Label>) {
        self.trailing.push(TrailingUnitReturn {
            frame,
            trailing,
            fallthrough,
        });
    }

    fn leave_trailing(&mut self) {
        self.trailing
            .pop()
            .expect("a unit inline-return frame was entered");
    }

    /// The fallthrough into `frame`'s closing brace when `transfer` is that frame's trailing break.
    fn fallthrough_for(&self, frame: &str, transfer: ExprId) -> Option<(ExprId, Label)> {
        let trailing = self
            .trailing
            .iter()
            .rev()
            .find(|candidate| candidate.frame == frame)?;
        if trailing.trailing != transfer {
            return None;
        }
        trailing.fallthrough.map(|label| (trailing.trailing, label))
    }
}

impl Emitter<'_> {
    /// Record the deferred result of an inline-return frame, when `declaration` declares one.
    pub(super) fn open_inline_return_frame(
        &mut self,
        declaration: ExprId,
        value: u32,
        ty: Ty,
    ) -> bool {
        if let Some(frame) = self.ir.inline_return_frames.get(&declaration) {
            self.inline_returns.insert_result(frame.clone(), value, ty);
            return true;
        }
        false
    }

    pub(super) fn inline_return_result(&self, frame: &str) -> Option<(u32, Ty)> {
        self.inline_returns.result(frame)
    }

    pub(super) fn has_inline_return_result(&self, frame: &str) -> bool {
        self.inline_returns.result(frame).is_some()
    }

    pub(super) fn inline_return_result_values(&self) -> HashSet<u32> {
        self.inline_returns.result_values().collect()
    }

    pub(super) fn inline_return_frame_result_type(&self, value: u32) -> Option<Ty> {
        self.inline_returns.result_type(value)
    }

    pub(super) fn unit_lambda_brace_depth(&self) -> usize {
        self.inline_returns.brace_len()
    }

    /// Remember that `frames` close with this lambda, so their loop exits bind after its brace.
    pub(super) fn open_unit_lambda_brace(&mut self, frames: Vec<String>) {
        if !frames.is_empty() {
            self.inline_returns.push_brace(frames, false);
        }
    }

    /// Like [`Self::open_unit_lambda_brace`], for a same-file lambda whose brace is its own return.
    pub(super) fn open_unit_lambda_return_brace(&mut self, frames: Vec<String>) {
        if !frames.is_empty() {
            self.inline_returns.push_brace(frames, true);
        }
    }

    /// The frame's loop exit stays unbound until this lambda's closing brace, when `frame` is one
    /// this lambda owns.
    pub(super) fn defer_unit_frame_loop_end(&mut self, frame: &str, end: Label) -> bool {
        self.inline_returns.note_end(frame, end)
    }

    /// Bind the loop exits this lambda deferred, in reverse order of opening.
    ///
    /// An exit that belongs after the lambda method's return stays open: the return has not been
    /// written yet.
    pub(super) fn bind_unit_lambda_braces(&mut self, depth: usize, code: &mut CodeBuilder) {
        while self.inline_returns.brace_len() > depth && !self.inline_returns.last_after_return() {
            for end in self.inline_returns.pop_brace() {
                self.bind(end, code);
            }
        }
    }

    /// Bind this builder's after-return loop exits, down to `depth`.
    ///
    /// An enclosing lambda's exit is left in place: its label belongs to that lambda's builder.
    /// Returns whether a bound exit is reachable, so the caller can give it an instruction.
    pub(super) fn bind_owned_unit_lambda_braces(
        &mut self,
        depth: usize,
        code: &mut CodeBuilder,
    ) -> bool {
        let mut landed = false;
        while self.inline_returns.brace_len() > depth {
            let Some(ends) = self.inline_returns.last_return_ends() else {
                break;
            };
            if ends.is_empty() || ends.iter().any(|end| !code.owns_label(*end)) {
                break;
            }
            for end in self.inline_returns.pop_brace() {
                self.bind(end, code);
                landed |= !code.is_dead();
            }
        }
        landed
    }

    pub(super) fn enter_unit_return_frame(
        &mut self,
        frame: String,
        trailing: ExprId,
        fallthrough: Option<Label>,
    ) {
        self.inline_returns
            .enter_trailing(frame, trailing, fallthrough);
    }

    pub(super) fn leave_unit_return_frame(&mut self) {
        self.inline_returns.leave_trailing();
    }

    pub(super) fn has_open_unit_lambda_brace(&self, frame: &str) -> bool {
        self.inline_returns.owns_frame(frame)
    }

    /// Where the synthetic trailing `break` of `frame` enters its closing brace.
    ///
    /// An earlier `break` of that frame is the labeled return. It keeps the loop exit resolved for
    /// the frame, which is bound after the brace.
    pub(super) fn unit_return_fallthrough(&self, transfer: ExprId, frame: &str) -> Option<Label> {
        let (trailing, fallthrough) = self.inline_returns.fallthrough_for(frame, transfer)?;
        match self.ir.expr(trailing) {
            IrExpr::Break { label: Some(found) } if found == frame => Some(fallthrough),
            _ => None,
        }
    }

    /// Claim a deferred result slot after the expression producing its first value has closed its
    /// nested local scopes. Ordinary locals already occupy their frame entry and keep their slot.
    pub(super) fn activate_inline_return_frame_result(&mut self, value: u32, ty: Ty) -> u16 {
        if self.inline_return_frame_result_type(value).is_none() {
            return self.slots[&value].0;
        }
        let slot = self
            .frame
            .activate_deferred(super::frame_map::FrameKey::Value(value));
        self.slots.insert(value, (slot, ty));
        slot
    }

    /// Store `value` as the result of the enclosing inline-return frame `frame`.
    pub(super) fn emit_set_frame_result(
        &mut self,
        frame: &str,
        value: ExprId,
        code: &mut CodeBuilder,
    ) {
        let Some((result, ty)) = self.inline_return_result(frame) else {
            self.run.set_emit_error(
                "a nested return stores the result of a frame that was never declared".to_string(),
            );
            return;
        };
        self.emit_value(value, code);
        // Coerced to the result's type as a store in the frame's own body is.
        self.adapt_physical_operand_for(value, self.value_ty(value), ty, code);
        // A frame outside the lambda node being compiled receives the result from the node's
        // marked return.
        if !self.diverges(value) && self.leave_lambda_node_with_result(frame, code) {
            return;
        }
        let slot = self.activate_inline_return_frame_result(result, ty);
        store(ty, slot, code);
        self.unassigned_values.remove(&result);
    }
}
