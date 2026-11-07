//! Emission state of an inline-return frame.
//!
//! An inline expansion leaves through `break` to its frame's label after storing the returned value
//! as the frame's result. A lambda body spliced inside the frame numbers its values separately, so a
//! return in it names the frame ([`IrExpr::SetFrameResult`]) rather than the result's value index.
//! The frame's result declaration records its value identity under the frame's label when it is
//! emitted; the first store activates that value's deferred JVM slot from whichever value domain
//! it runs in.
//!
//! A `Unit` frame has no result slot. Its labeled return is still a `break` of the frame, and that
//! break lands on the instruction after the spliced lambda's closing-brace `nop`. The join is keyed
//! by the frame's label: a nested lambda pushes its own join, and an outer return must not take it.

use super::*;

/// Results and closing-brace joins of the inline-return frames in the method being emitted.
#[derive(Default)]
pub(super) struct InlineReturnState {
    /// Deferred result `(value, type)` of each value-producing frame, by the frame's label.
    results: HashMap<String, (u32, Ty)>,
    /// Brace joins of spliced lambdas, innermost last. Each names the `Unit` frames whose early
    /// `break` lands on `at`, just after that lambda's closing-brace `nop`.
    braces: Vec<UnitLambdaBrace>,
    /// Trailing synthetic breaks of `Unit` frames whose bodies are being emitted, innermost last.
    trailing: Vec<(String, ExprId)>,
}

struct UnitLambdaBrace {
    frames: Vec<String>,
    at: Label,
    /// The join is the instruction after this lambda method's own return. The return is what the
    /// inliner turns into the closing-brace `nop`, so the join cannot be bound in the body.
    after_return: bool,
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

    fn push_brace(&mut self, frames: Vec<String>, at: Label, after_return: bool) {
        self.braces.push(UnitLambdaBrace {
            frames,
            at,
            after_return,
        });
    }

    fn brace_len(&self) -> usize {
        self.braces.len()
    }

    /// The innermost join, when it belongs after the lambda method's return.
    fn last_return_brace(&self) -> Option<Label> {
        self.braces
            .last()
            .filter(|brace| brace.after_return)
            .map(|brace| brace.at)
    }

    fn last_after_return(&self) -> bool {
        self.braces.last().is_some_and(|brace| brace.after_return)
    }

    fn pop_brace(&mut self) -> Label {
        self.braces
            .pop()
            .expect("a unit lambda brace join was pushed")
            .at
    }

    /// The join after the lambda that owns `frame`, when one is open.
    fn brace_for(&self, frame: &str) -> Option<Label> {
        self.braces
            .iter()
            .rev()
            .find(|brace| brace.frames.iter().any(|candidate| candidate == frame))
            .map(|brace| brace.at)
    }

    fn enter_trailing(&mut self, frame: String, trailing: ExprId) {
        self.trailing.push((frame, trailing));
    }

    fn leave_trailing(&mut self) {
        self.trailing
            .pop()
            .expect("a unit inline-return frame was entered");
    }

    fn trailing_for(&self, frame: &str) -> Option<ExprId> {
        self.trailing
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == frame)
            .map(|(_, trailing)| *trailing)
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

    /// Remember that early breaks of `frames` land on `at`, after this lambda's closing brace.
    pub(super) fn open_unit_lambda_brace(&mut self, frames: Vec<String>, at: Label) {
        if !frames.is_empty() {
            self.inline_returns.push_brace(frames, at, false);
        }
    }

    /// Like [`Self::open_unit_lambda_brace`], for a same-file lambda whose brace is its own return.
    pub(super) fn open_unit_lambda_return_brace(&mut self, frames: Vec<String>, at: Label) {
        if !frames.is_empty() {
            self.inline_returns.push_brace(frames, at, true);
        }
    }

    /// Bind the brace joins this lambda pushed, in reverse order of opening.
    ///
    /// A join that belongs after the lambda method's return stays open: the return has not been
    /// written yet, and binding the join here would put it on the `Unit` the return then consumes.
    pub(super) fn bind_unit_lambda_braces(&mut self, depth: usize, code: &mut CodeBuilder) {
        while self.inline_returns.brace_len() > depth && !self.inline_returns.last_after_return() {
            let at = self.inline_returns.pop_brace();
            self.bind(at, code);
        }
    }

    /// Bind this builder's after-return joins, down to `depth`.
    ///
    /// An enclosing lambda's join is left in place: its label belongs to that lambda's builder.
    /// Returns whether a bound join is reachable, so the caller can give it an instruction.
    pub(super) fn bind_owned_unit_lambda_braces(
        &mut self,
        depth: usize,
        code: &mut CodeBuilder,
    ) -> bool {
        let mut landed = false;
        while self.inline_returns.brace_len() > depth {
            let Some(at) = self.inline_returns.last_return_brace() else {
                break;
            };
            if !code.owns_label(at) {
                break;
            }
            let at = self.inline_returns.pop_brace();
            self.bind(at, code);
            landed |= !code.is_dead();
        }
        landed
    }

    pub(super) fn enter_unit_return_frame(&mut self, frame: String, trailing: ExprId) {
        self.inline_returns.enter_trailing(frame, trailing);
    }

    pub(super) fn leave_unit_return_frame(&mut self) {
        self.inline_returns.leave_trailing();
    }

    pub(super) fn has_open_unit_lambda_brace(&self, frame: &str) -> bool {
        self.inline_returns.brace_for(frame).is_some()
    }

    pub(super) fn unit_lambda_brace(&self, frame: &str) -> Option<Label> {
        self.inline_returns.brace_for(frame)
    }

    /// Where an early `break` of the `Unit` frame `frame` lands: past that frame's lambda brace.
    ///
    /// The frame's own trailing break is not early; it falls into the brace `nop`. A frame with
    /// no open join, or whose trailing break is not the one recorded for it, keeps the loop exit.
    pub(super) fn unit_return_past_brace(&self, transfer: ExprId, frame: &str) -> Option<Label> {
        let at = self.inline_returns.brace_for(frame)?;
        let trailing = self.inline_returns.trailing_for(frame)?;
        if trailing == transfer {
            return None;
        }
        match self.ir.expr(trailing) {
            IrExpr::Break { label: Some(found) } if found == frame => Some(at),
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
