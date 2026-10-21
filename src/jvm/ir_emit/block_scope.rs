//! JVM emission for common-IR lexical blocks and their local-slot lifetimes.

use crate::fx_hash::HashMap;

use super::declaration_types::jvm_is_erased_top;
use super::frame_map::{FrameKey, Mark, TempRole, TempSlot};
use super::{ir_ty_to_jvm, CodeBuilder, Emitter, Label, Ty};
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
        let has_lambda_frame = self.opens_spliced_lambda_frame(&stmts);
        let normalizes_inline_stack = inline_stack_height != 0
            && (self.ir.external_inline_expansions.contains(&block)
                || (self.ir.is_inline_copy(block) && has_lambda_frame));
        let within_inline_lambda = normalizes_inline_stack && self.within_inline_lambda_frame();
        // A spliced lambda's marker follows evaluation of the inline call's ordinary operands.
        // Preserve the caller stack there so argument temporaries keep kotlinc's slot order. An
        // expansion without a lambda marker (for example an iteration plan) still starts at the
        // block boundary.
        let delays_stack_normalization = normalizes_inline_stack && has_lambda_frame;
        if normalizes_inline_stack && !delays_stack_normalization {
            open_inline_stack(code);
        }
        self.link_safe_call_chain(block, code);
        let copied_caller = self.copied_function_caller(block, code);
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
            retains_postfix_temporary,
            reserved_inline_stack,
            close_lambda_before_locals,
            emit_after_frame_close,
            discard_after_frame,
            close_discarded_provider_frame,
        } = if self.try_emit_duplicated_safe_call(block, &stmts, value, true, code) {
            self.terminal_statement_target = None;
            self.statement_line = enclosing_statement_line;
            EmittedBlock {
                discard_after_close: None,
                retains_postfix_temporary: false,
                reserved_inline_stack: None,
                close_lambda_before_locals: false,
                emit_after_frame_close: None,
                discard_after_frame: None,
                close_discarded_provider_frame: None,
            }
        } else {
            self.block_depth -= 1;
            let entry = InlineBodyEntry {
                normalize_at_lambda_frame: delays_stack_normalization,
                stack_height: inline_stack_height,
                within_inline_lambda,
                opening: self.external_inline_body_opening(block, &stmts),
                copied_function: copied_caller.is_some().then_some(block),
                discarded_frame_result: value
                    .and_then(|value| self.discarded_copied_frame_tail(block, &stmts, value)),
            };
            self.emit_open_block(block, stmts, value, terminal_target, entry, true, code)
        };
        self.mark_initializer_line(block, true, code);
        let mut anchored = false;
        if !self.ir.callable_scopes.contains(&block) {
            self.close_external_inline_frame(block, code);
            if close_lambda_before_locals {
                self.close_spliced_lambda_frame(block, true, code);
            } else if discard_after_close.is_some() {
                // A discarded `return this` (`also`/`apply`). A multi-line `}` is a `nop`
                // while the lambda locals are still open; a one-line brace shares the body's
                // line and the sweep drops the `nop`. The call-line `nop` still follows the
                // locals.
                self.emit_spliced_lambda_brace(block, code);
            }
            if let Some(invocation) = close_discarded_provider_frame {
                self.close_provider_invocation_frame(invocation, code);
            }
            self.close_scope_locals(code, marked_initializer);
            if close_discarded_provider_frame.is_none() {
                anchored = self.close_copied_function_inline_frame(block, code);
            }
        }
        // kotlinc returns to the call's line once the inlined body ends, before the statement
        // discards its result: the `pop` occupies that line, so the cleanup drops the frame's
        // closing `nop`.
        let frame_closed_before_discard =
            discard_after_frame.is_some() && !self.ir.callable_scopes.contains(&block);
        if let Some(words) = discard_after_frame {
            if frame_closed_before_discard {
                self.close_spliced_lambda_frame(block, false, code);
            }
            discard_stack_words(words, code);
        }
        if let Some(expression) = emit_after_frame_close {
            self.emit_discarding(expression, code);
        }
        if normalizes_inline_stack
            && (!delays_stack_normalization || reserved_inline_stack.is_some())
        {
            close_inline_stack(inline_stack_height, code);
        }
        // The lambda locals end at this `nop`. It returns to the inline body's line, and that
        // local-end label is what keeps it when the following instruction is the next debug point.
        // A statement `Unit` lambda already closed on its brace `nop`; the inline body's line
        // still follows its locals, where that body discards the `Unit` result. A copied body's
        // terminal `Unit` that keeps its own mapped line has already written this `nop`.
        let unit_wrote_body_line = emit_after_frame_close
            .is_some_and(|unit| self.has_retained_mapped_inline_unit_line(unit));
        if !self.ir.callable_scopes.contains(&block)
            && !unit_wrote_body_line
            && !frame_closed_before_discard
        {
            self.close_spliced_lambda_frame(block, false, code);
        }
        if retains_postfix_temporary {
            // kotlinc closes the postfix block between its result load and discard. Keep that
            // ordering through the trivial-load sweep; the final nop sweep removes this anchor.
            code.nop();
        }
        if let Some(discarded) = discard_after_close {
            super::discard(self.value_ty(discarded), code);
        }
        self.block_depth -= 1;
        self.restore_slot_scope(saved);
        self.release_reserved_inline_stack(reserved_inline_stack);
        // A statement-position inline expansion ran under the callee. Forget the line in effect so
        // the next mark is written even when it names that same line: a one-line `Unit` body marks
        // its closing `return` again (`markLineNumberAfterInlineIfNeeded`).
        self.mark_after_inlined_call(block, code);
        self.leave_copied_function_body(copied_caller, anchored, code);
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
        let has_lambda_frame = self.opens_spliced_lambda_frame(stmts);
        let normalizes_inline_stack = inline_stack_height != 0
            && (self.ir.external_inline_expansions.contains(&block)
                || (self.ir.is_inline_copy(block) && has_lambda_frame));
        let within_inline_lambda = normalizes_inline_stack && self.within_inline_lambda_frame();
        let delays_stack_normalization = normalizes_inline_stack && has_lambda_frame;
        if normalizes_inline_stack && !delays_stack_normalization {
            open_inline_stack(code);
        }
        self.link_safe_call_chain(block, code);
        let copied_caller = self.copied_function_caller(block, code);
        let enclosing_statement_line = self.statement_line;
        let saved = self.open_slot_scope();
        self.block_depth += 1;
        let mut dead = false;
        let mut reserved_inline_stack = None;
        let mut anchored_lambda_result = false;
        let mut deferred_invocation_result = None;
        let mut copied_frame_result = None;
        let optimized_postfix = self.emit_postfix_iinc(block, false, code);
        if !optimized_postfix
            && !self.try_emit_duplicated_safe_call(block, stmts, value, false, code)
        {
            let mut normalize_at_lambda_frame = delays_stack_normalization;
            let opening = self.external_inline_body_opening(block, stmts);
            let mut bound_operands = 0;
            for (index, &statement) in stmts.iter().enumerate() {
                if bound_operands > 0 {
                    bound_operands -= 1;
                    continue;
                }
                self.open_external_inline_body(opening, statement, code);
                self.enter_copied_function_frame(
                    copied_caller.is_some().then_some(block),
                    statement,
                    code,
                );
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
                    reserved_inline_stack = Some(self.open_inline_stack_at_lambda_frame(
                        inline_stack_height,
                        within_inline_lambda,
                        code,
                    ));
                    normalize_at_lambda_frame = false;
                }
                // A statement nets zero on the operand stack (its value is stored/discarded). Reset the
                // tracked height to that baseline afterward: raw spliced control flow is opaque to the
                // builder's linear counter and can leave `cur_stack` drifted above the real,
                // verified-balanced height. Later emission still relies on accurate physical stack
                // accounting even though final-body analysis owns verifier frames.
                let base = code.stack_height();
                bound_operands = self.emit_invocation_operand_bindings(&stmts[index..], code);
                if bound_operands > 0 {
                    bound_operands -= 1;
                    code.set_stack(base.max(0) as u16);
                    continue;
                }
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
                        if let Some((effects, unit)) = self.unit_after_lambda_effects(stmts, value)
                        {
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
                                self.close_or_defer_inline_stack(
                                    block,
                                    inline_stack_height,
                                    &mut reserved_inline_stack,
                                    code,
                                );
                            }
                            self.block_depth -= 1;
                            self.restore_slot_scope(saved);
                            self.release_reserved_inline_stack(reserved_inline_stack);
                            self.statement_line = enclosing_statement_line;
                            self.leave_copied_function_body(copied_caller, false, code);
                            return;
                        }
                    }
                    // `also`/`apply` end in `return this`. kotlinc closes the spliced lambda
                    // before that load, and the load itself carries the call's line. A `nop`
                    // after the load sits between the value and its consumer (`astore` of the
                    // initializer, or the join of an `if`) and is not in kotlinc's code.
                    if self.spliced_lambda_result_read(stmts, value)
                        && !self.ir.callable_scopes.contains(&block)
                    {
                        self.close_external_inline_frame(block, code);
                        // A multi-line lambda's `}` is a `nop` while its locals are still open.
                        // A one-line brace shares the body's line; the `nop` is then redundant
                        // and the trivial sweep drops it.
                        self.emit_spliced_lambda_brace(block, code);
                        self.close_scope_locals(code, false);
                        self.anchor_spliced_lambda_result(block, code);
                        self.emit_value(value, code);
                        anchored_lambda_result = true;
                    } else if self.ir.external_frame_closes.contains_key(&block) {
                        if let Some(ret) =
                            self.emit_invocation_before_result_adaptation(value, code)
                        {
                            deferred_invocation_result = Some((value, ret));
                        } else {
                            self.emit_value(value, code);
                        }
                    } else if let Some(result) = self.copied_frame_result(block, value) {
                        copied_frame_result = Some(
                            self.emit_copied_frame_result_before_adaptation(result, value, code),
                        );
                    } else {
                        self.emit_value(value, code);
                    }
                }
            }
        }
        if !dead && !optimized_postfix && !anchored_lambda_result {
            if let Some(value) = value.filter(|_| self.opens_spliced_lambda_frame(stmts)) {
                self.mark_lambda_result_return(value, code);
            }
        }
        // A callable's own scope is its body. A value-returning lambda keeps those locals
        // open through the return, the same way a statement-position callable scope does.
        let mut anchored = false;
        if !anchored_lambda_result && !self.ir.callable_scopes.contains(&block) {
            self.adapt_pending_copied_result_before_lambda_close(stmts, value, code);
            self.close_external_inline_frame(block, code);
            if let Some(result) = copied_frame_result {
                self.close_provider_result_frame(result, code);
            }
            self.close_scope_locals(code, false);
            if !copied_frame_result.is_some_and(CopiedFrameResult::is_provider) {
                anchored = self.close_copied_function_inline_frame(block, code);
            }
        }
        if let Some(result) = copied_frame_result {
            self.adapt_copied_frame_result(result, code);
        }
        if normalizes_inline_stack
            && (!delays_stack_normalization || reserved_inline_stack.is_some())
        {
            self.close_or_defer_inline_stack(
                block,
                inline_stack_height,
                &mut reserved_inline_stack,
                code,
            );
        }
        // After the lambda locals, including when this block is itself a callable scope and those
        // locals stay open through the return. The `nop` is the inline body's line, not the brace.
        // A `return this` already anchored that line on the load.
        if !anchored_lambda_result {
            if let Some(pending) = self
                .inline_coercions
                .pending_for(block)
                .filter(|pending| pending.saved.is_some())
            {
                pending.closes_spliced_frame = true;
            } else {
                self.close_spliced_lambda_frame(block, false, code);
            }
        }
        if let Some((invocation, ret)) = deferred_invocation_result {
            self.finish_function_invocation_result(invocation, ret, code);
        }
        self.block_depth -= 1;
        self.restore_slot_scope(saved);
        self.release_reserved_inline_stack(reserved_inline_stack);
        self.statement_line = enclosing_statement_line;
        self.leave_copied_function_body(copied_caller, anchored, code);
    }

    /// A copied inline-function frame result the caller physically adapts after the frame's
    /// locals end. Common IR marks the exact same-module result boundary; a nested same-module or
    /// provider-planned external boundary owns its own trailing result instead.
    fn copied_frame_result(&self, block: u32, value: u32) -> Option<CopiedFrameResult> {
        // A safe call whose selector opens a spliced lambda frame joins that selector with its
        // null arm after the copied frame's closing nop. kotlinc narrows an erased invocation
        // before that nop. Unit materialization and other copied results still follow their
        // ordinary after-frame path.
        let adapts_before_lambda_close = self.active_safe_call_selector_contains(block)
            && self.trailing_value_opens_spliced_lambda_frame(value);
        let tail = self.copied_frame_tail(block, value)?;
        let expression = tail.expression();
        match *self.ir.expr(expression) {
            IrExpr::UnitInstance if tail.is_owned() => Some(CopiedFrameResult::Unit),
            IrExpr::InvokeFunction { ret, .. }
                if tail.is_owned()
                    && self.erases_copied_frame_result(block)
                    && self
                        .ir
                        .invocation_result_adaptation_line(expression)
                        .is_none()
                    && !matches!(ret, Ty::Unit | Ty::Nothing)
                    && self.transformed_result(expression).is_none()
                    && !self.erased_invocations.contains(&expression) =>
            {
                Some(CopiedFrameResult::Invocation {
                    invocation: expression,
                    ret,
                    adapts_before_lambda_close,
                })
            }
            IrExpr::InvokeFunction { ret, .. }
                if !tail.is_owned()
                    && self
                        .ir
                        .invocation_result_adaptation_line(expression)
                        .is_some()
                    && !matches!(ret, Ty::Unit | Ty::Nothing)
                    && self.transformed_result(expression).is_none()
                    && !self.erased_invocations.contains(&expression) =>
            {
                let caller_line = self
                    .ir
                    .inline_copy_provenance(expression)?
                    .last()?
                    .call_line?;
                Some(CopiedFrameResult::ProviderInvocation {
                    invocation: expression,
                    ret,
                    caller_line,
                })
            }
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } if tail.is_owned()
                && self.erases_copied_frame_result(block)
                && self.transformed_result(expression).is_none() =>
            {
                Some(CopiedFrameResult::Coercion {
                    expression,
                    arg,
                    type_operand,
                    physical_arg: None,
                })
            }
            _ => None,
        }
    }

    /// Follow only transparent trailing-value blocks inside one explicitly recorded copied result
    /// boundary. A nested copied boundary keeps ownership of its own result. A provider-planned
    /// external boundary keeps adaptation ownership too, but allows the enclosing copied frame to
    /// delay that adaptation until its own locals have ended.
    fn copied_frame_tail(&self, block: u32, value: u32) -> Option<CopiedFrameTail> {
        if !self.ir.is_inline_result_boundary(block) {
            return None;
        }
        let mut tail = value;
        let mut provider_owned = false;
        while let IrExpr::Block {
            value: Some(inner), ..
        } = self.ir.expr(tail)
        {
            if self.ir.is_inline_result_boundary(tail) {
                return None;
            }
            provider_owned |= self.ir.external_inline_expansions.contains(&tail);
            tail = *inner;
        }
        Some(if provider_owned {
            CopiedFrameTail::Provider(tail)
        } else {
            CopiedFrameTail::Owned(tail)
        })
    }

    /// The active safe-call selector may retain result coercions around its copied inline body.
    /// Follow only those transparent coercions to identify that exact body.
    fn active_safe_call_selector_contains(&self, block: u32) -> bool {
        let Some(mut selector) = self.active_safe_call_selector else {
            return false;
        };
        loop {
            if selector == block {
                return true;
            }
            match self.ir.expr(selector) {
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => selector = *arg,
                _ => return false,
            }
        }
    }

    /// A copied function body can wrap the spliced lambda that produces its trailing result in
    /// transparent value blocks. The lambda frame, rather than the outer function frame, is what
    /// makes kotlinc narrow the safe-call selector before the copied frame closes.
    fn trailing_value_opens_spliced_lambda_frame(&self, mut value: u32) -> bool {
        loop {
            let IrExpr::Block {
                stmts,
                value: Some(inner),
            } = self.ir.expr(value)
            else {
                return false;
            };
            if self.opens_spliced_lambda_frame(stmts) {
                return true;
            }
            value = *inner;
        }
    }

    fn adapt_pending_copied_result_before_lambda_close(
        &mut self,
        stmts: &[u32],
        value: Option<u32>,
        code: &mut CodeBuilder,
    ) {
        if !self.opens_spliced_lambda_frame(stmts) {
            return;
        }
        let Some(pending) = self.inline_coercions.pre_close else {
            return;
        };
        let Some(mut tail) = value else {
            return;
        };
        loop {
            if tail == pending.invocation {
                self.inline_coercions.pre_close = None;
                if !code.is_dead() {
                    self.narrow_invocation_result(pending.ret, code);
                }
                return;
            }
            match self.ir.expr(tail) {
                IrExpr::Block {
                    value: Some(inner), ..
                } => tail = *inner,
                _ => return,
            }
        }
    }

    /// The trailing value of a discarded copied result boundary. It is emitted while the copied
    /// frame is open and popped only after the frame's locals have ended.
    pub(super) fn discarded_copied_frame_result(
        &self,
        block: u32,
        stmts: &[u32],
        value: u32,
    ) -> Option<u32> {
        self.discarded_copied_frame_tail(block, stmts, value)
            .map(CopiedFrameTail::expression)
    }

    fn discarded_copied_frame_tail(
        &self,
        block: u32,
        _stmts: &[u32],
        value: u32,
    ) -> Option<CopiedFrameTail> {
        let tail = self.copied_frame_tail(block, value)?;
        let provider_owned = !tail.is_owned();
        let expression = tail.expression();
        // Select every semantic value here; emission below measures its actual JVM stack width.
        // Restricting this to calls moves a copied body's field/local/static/constructor pop back
        // inside the frame even though each of those tails physically leaves a value too. The
        // transparent carrier blocks may contain effects: emitting the complete value retains its
        // result on the stack across the enclosing copied boundary. A provider-planned invocation
        // similarly retains its own adaptation identity, but a discarded outer call suppresses
        // that consumer-only adaptation and owns the eventual pop.
        let produces_value = !matches!(
            self.value_ty(expression),
            Ty::Unit | Ty::Nothing | Ty::Error
        );
        (produces_value
            && !self.diverges(expression)
            && self.transformed_result(expression).is_none()
            && !self.machine_suspensions.contains_key(&expression)
            && (provider_owned
                || self
                    .ir
                    .invocation_result_adaptation_line(expression)
                    .is_none()))
        .then_some(tail)
    }

    /// Emit a discarded copied frame's trailing value and return the physical JVM stack width to
    /// pop after the frame ends. A function-value invocation stays erased because it has no
    /// consumer that needs its checked narrowing.
    fn emit_discarded_frame_result(
        &mut self,
        value: u32,
        result: u32,
        code: &mut CodeBuilder,
    ) -> Option<u16> {
        let erased = matches!(self.ir.expr(result), IrExpr::InvokeFunction { .. });
        if erased {
            self.erased_invocations.insert(result);
        }
        let before = code.stack_height();
        self.emit_value(value, code);
        self.erased_invocations.remove(&result);
        let words = code.stack_height() - before;
        (!code.is_dead() && matches!(words, 1 | 2)).then_some(words as u16)
    }

    /// The implicit `return` of an inlined lambda's trailing value. kotlinc marks a return's line
    /// once its value is on the stack, at the expression's end, and the inliner turns the return
    /// into a `nop`. The mark repeats the line in effect, so it writes nothing, unless the value
    /// ended on another line or an inline call inside it reset the line
    /// (`markLineNumberAfterInlineIfNeeded`): then the `nop` is that line's only instruction and
    /// the cleanup keeps it.
    fn mark_lambda_result_return(&mut self, value: u32, code: &mut CodeBuilder) {
        if code.is_dead() {
            return;
        }
        // The lambda body's block, and a coercion of it to the lambda's return type, carry the
        // lambda's `}`; the return is its trailing expression's. An inlined call is that
        // expression itself, not a body to look into.
        let mut returned = value;
        loop {
            returned = match *self.ir.expr(returned) {
                IrExpr::Block {
                    value: Some(inner), ..
                } if !self.realizes_inlined_call(returned) => inner,
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => arg,
                _ => break,
            };
        }
        let Some(line) = self
            .ir
            .expr_end_lines
            .get(&returned)
            .or_else(|| self.ir.expr_source_lines.get(&returned))
            .copied()
            .filter(|line| *line != 0)
        else {
            return;
        };
        let before = code.line_marks().last().copied();
        self.mark_expression_line(returned, line, code);
        if code.line_marks().last().copied() != before {
            code.nop();
        }
    }

    /// Whether an inlined lambda's trailing value is a value the JVM stack carries out of the
    /// lambda frame. A `when` discards in its branches, as kotlinc's pop propagation leaves it.
    fn leaves_lambda_result(&self, value: u32) -> bool {
        !matches!(self.value_ty(value), Ty::Unit | Ty::Nothing | Ty::Error)
            && !matches!(self.ir.expr(value), IrExpr::When { .. })
            && !self.diverges(value)
            && self.transformed_result(value).is_none()
            && !self.machine_suspensions.contains_key(&value)
    }

    fn erases_copied_frame_result(&self, block: u32) -> bool {
        self.ir
            .inline_declared_type(block)
            .is_some_and(|declared| jvm_is_erased_top(ir_ty_to_jvm(&declared)))
    }

    fn emit_copied_frame_result_before_adaptation(
        &mut self,
        result: CopiedFrameResult,
        value: u32,
        code: &mut CodeBuilder,
    ) -> CopiedFrameResult {
        match result {
            CopiedFrameResult::Unit => {
                self.emit_discarding(value, code);
                result
            }
            CopiedFrameResult::Invocation {
                invocation,
                ret,
                adapts_before_lambda_close,
            } => {
                self.erased_invocations.insert(invocation);
                let enclosing = self.inline_coercions.pre_close.take();
                self.inline_coercions.pre_close = adapts_before_lambda_close
                    .then_some(PendingPreCloseCopiedResult { invocation, ret });
                self.emit_value(value, code);
                let unconsumed = self.inline_coercions.pre_close.take();
                self.inline_coercions.pre_close = enclosing;
                if unconsumed.is_some() && !code.is_dead() {
                    self.run.set_emit_error(
                        "a safe-call copied result did not reach its trailing lambda frame"
                            .to_string(),
                    );
                }
                result
            }
            CopiedFrameResult::ProviderInvocation { invocation, .. } => {
                self.erased_invocations.insert(invocation);
                self.emit_value(value, code);
                result
            }
            CopiedFrameResult::Coercion {
                expression,
                arg,
                type_operand,
                physical_arg: _,
            } => {
                if !self.inline_coercions.begin_deferred(expression) {
                    self.run.set_emit_error(
                        "a copied inline-result coercion recursively reused one expression"
                            .to_string(),
                    );
                    return result;
                }
                self.emit_value(value, code);
                let physical_arg = self.inline_coercions.finish_deferred(expression);
                if physical_arg.is_none() && !code.is_dead() {
                    self.run.set_emit_error(
                        "a copied inline-result coercion was not reached through its trailing value"
                            .to_string(),
                    );
                }
                CopiedFrameResult::Coercion {
                    expression,
                    arg,
                    type_operand,
                    physical_arg,
                }
            }
        }
    }

    fn adapt_copied_frame_result(&mut self, result: CopiedFrameResult, code: &mut CodeBuilder) {
        match result {
            CopiedFrameResult::Unit => {
                if !code.is_dead() {
                    let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
                    code.getstatic(unit, 1);
                }
            }
            CopiedFrameResult::Invocation {
                invocation,
                ret,
                adapts_before_lambda_close,
            } => {
                if adapts_before_lambda_close {
                    self.erased_invocations.remove(&invocation);
                    return;
                }
                if !self.erased_invocations.remove(&invocation) && !code.is_dead() {
                    self.narrow_invocation_result(ret, code);
                }
            }
            CopiedFrameResult::ProviderInvocation {
                invocation: _,
                ret,
                caller_line,
            } => {
                if !code.is_dead() {
                    // The enclosing copied boundary, rather than the provider expansion itself,
                    // now precedes the adaptation. Restore the containing same-file call line from
                    // its resolved copy provenance here. The provider's own recorded line names
                    // the inline declaration before this extra copied-frame boundary.
                    code.forget_line();
                    code.mark_line(caller_line);
                    self.narrow_invocation_result(ret, code);
                }
            }
            CopiedFrameResult::Coercion {
                expression: _,
                arg,
                type_operand,
                physical_arg,
            } => {
                if let Some(physical_arg) = physical_arg.filter(|_| !code.is_dead()) {
                    self.emit_deferred_inline_coercion(arg, type_operand, physical_arg, code);
                }
            }
        }
    }

    /// A provider result delayed through a copied same-file frame closes that containing frame on
    /// the copied declaration's mapped line while its marker locals are still open. The provider's
    /// caller-owned adaptation follows after those locals end.
    fn close_provider_result_frame(&mut self, result: CopiedFrameResult, code: &mut CodeBuilder) {
        let CopiedFrameResult::ProviderInvocation { invocation, .. } = result else {
            return;
        };
        self.close_provider_invocation_frame(invocation, code);
    }

    fn close_provider_invocation_frame(&mut self, invocation: u32, code: &mut CodeBuilder) {
        if code.is_dead() {
            return;
        }
        let line = self
            .ir
            .invocation_result_adaptation_line(invocation)
            .expect("a provider-owned result must retain its declaration line");
        code.forget_line();
        self.mark_expression_line(invocation, line, code);
        code.nop();
    }

    /// An inlined `return this` (`also`, `apply`): the block's value reads a local this block
    /// declared, and the block opened a spliced lambda frame. A read of an outer capture is the
    /// lambda's own value and stays inside the frame.
    fn spliced_lambda_result_read(&self, stmts: &[u32], value: u32) -> bool {
        let IrExpr::GetValue(slot) = self.ir.expr(value) else {
            return false;
        };
        self.opens_spliced_lambda_frame(stmts)
            && stmts.iter().any(|&statement| {
                matches!(
                    self.ir.expr(statement),
                    IrExpr::Variable { index, .. } if *index == *slot
                ) && !matches!(
                    self.ir.debug_local_provenance(statement),
                    Some(
                        crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { .. }
                            | crate::ir::IrDebugLocalProvenance::InlineLambdaContinuation { .. }
                    )
                )
            })
    }

    /// The spliced lambda's closing brace, while its locals are still open.
    fn emit_spliced_lambda_brace(&mut self, block: u32, code: &mut CodeBuilder) {
        let Some(line) = self
            .ir
            .expr_end_lines
            .get(&block)
            .copied()
            .filter(|line| *line != 0)
        else {
            return;
        };
        if self
            .ir
            .inline_copy_provenance(block)
            .is_some_and(|frames| frames.len() > 1)
        {
            self.mark_expression_line(block, line, code);
        } else {
            code.mark_line(line);
        }
        code.nop();
    }

    /// Put the inline call's line on the instruction that follows, without a `nop`. The result
    /// load is that instruction; kotlinc's line table records the call there.
    fn anchor_spliced_lambda_result(&mut self, block: u32, code: &mut CodeBuilder) {
        let Some(&line) = self.ir.expr_source_lines.get(&block) else {
            return;
        };
        code.forget_line();
        self.mark_expression_line(block, line, code);
    }

    /// A copied inline-function expansion returns to its own line before the enclosing caller
    /// continues. The body's last instruction is on the callee's line; the caller's next
    /// instruction may be on another, so this frame's line needs an anchor of its own. A later
    /// instruction on the same line drops the `nop`.
    ///
    /// Returns whether it anchored that line.
    fn close_copied_function_inline_frame(&mut self, block: u32, code: &mut CodeBuilder) -> bool {
        if code.is_dead()
            || self.ir.inline_copy_provenance(block).is_none()
            || !self.opens_function_frame(block)
        {
            return false;
        }
        let Some(line) = self
            .ir
            .expr_source_lines
            .get(&block)
            .copied()
            .filter(|line| *line != 0)
        else {
            return false;
        };
        self.mark_expression_line(block, line, code);
        code.nop();
        true
    }

    /// Whether `block` opens an inline function's frame: it is that call's expansion.
    fn opens_function_frame(&self, block: u32) -> bool {
        let IrExpr::Block { stmts, .. } = self.ir.expr(block) else {
            return false;
        };
        stmts.iter().any(|&statement| {
            matches!(
                self.ir.debug_local_provenance(statement),
                Some(IrDebugLocalProvenance::FunctionFrameMarker)
            )
        })
    }

    /// Capture the caller state for a same-module inline function's expansion. Operand evaluation
    /// and binding still belong to the caller; [`Self::enter_copied_function_frame`] suspends the
    /// condition state only when the function-frame marker starts the copied callee body.
    fn copied_function_caller(
        &self,
        block: u32,
        code: &CodeBuilder,
    ) -> Option<CopiedFunctionCaller> {
        if self.ir.external_inline_expansions.contains(&block) || !self.opens_function_frame(block)
        {
            return None;
        }
        Some(CopiedFunctionCaller {
            line: code.current_line(),
            inside_condition: self.inside_condition,
            operand_binding: self.copied_operand_binding,
        })
    }

    /// Cross from a same-module inline call's operand bindings into its copied callee body.
    ///
    /// The operands are kotlinc's `genValueAndPut`: each is stored straight into its parameter
    /// slot, with no line of its own. The call then marks its own line, after its operands
    /// (`visitCall`), and the inliner opens the body with a `nop` there. That `nop` survives where
    /// the call's line starts a debug range of its own (an operand on another line, or one that ran
    /// an inlined body), so the parameters' ranges start after it.
    fn enter_copied_function_frame(
        &mut self,
        copied_function: Option<u32>,
        statement: u32,
        code: &mut CodeBuilder,
    ) {
        let Some(block) = copied_function else {
            return;
        };
        let opens_frame = matches!(
            self.ir.debug_local_provenance(statement),
            Some(IrDebugLocalProvenance::FunctionFrameMarker)
        );
        if !opens_frame {
            self.copied_operand_binding = Some(statement);
            return;
        }
        self.inside_condition = false;
        if code.is_dead() {
            return;
        }
        if let Some(line) = self
            .ir
            .expr_source_lines
            .get(&block)
            .copied()
            .filter(|l| *l != 0)
        {
            self.mark_expression_line(block, line, code);
            code.nop();
        }
    }

    /// kotlinc's `markLineNumberAfterInlineIfNeeded` after a same-module inline expansion: inside a
    /// condition the caller's line is written again for the jump that follows. Elsewhere the
    /// expansion's frame closing already places the caller's next line, and a frame that anchored
    /// its own line already restored the caller.
    fn leave_copied_function_body(
        &mut self,
        caller: Option<CopiedFunctionCaller>,
        anchored: bool,
        code: &mut CodeBuilder,
    ) {
        let Some(caller) = caller else {
            return;
        };
        self.inside_condition = caller.inside_condition;
        self.copied_operand_binding = caller.operand_binding;
        if anchored || code.is_dead() {
            return;
        }
        if let Some(line) = caller.line.filter(|_| caller.inside_condition) {
            code.inlined_line(line);
        }
    }

    /// kotlinc's `InlineCodegen` starts every inlined body with a `nop` on the call's line, after
    /// the call's operands and before the body's first frame marker. The redundant-`nop` sweep
    /// keeps it only when that line has no other instruction: `run { 1 }` keeps it, while
    /// `value.let { ... }` loses it to the receiver load already on that line.
    fn external_inline_body_opening(&self, block: u32, stmts: &[u32]) -> Option<InlineBodyOpening> {
        if !self.ir.external_inline_expansions.contains(&block) {
            return None;
        }
        let line = self
            .ir
            .expr_source_lines
            .get(&block)
            .copied()
            .filter(|line| *line != 0)?;
        let marker = stmts.iter().copied().find(|&statement| {
            matches!(
                self.ir.debug_local_provenance(statement),
                Some(
                    IrDebugLocalProvenance::FunctionFrameMarker
                        | IrDebugLocalProvenance::LambdaFrameMarker { .. }
                )
            )
        })?;
        Some(InlineBodyOpening {
            block,
            marker,
            line,
        })
    }

    fn open_external_inline_body(
        &mut self,
        opening: Option<InlineBodyOpening>,
        statement: u32,
        code: &mut CodeBuilder,
    ) {
        let Some(opening) = opening.filter(|opening| opening.marker == statement) else {
            return;
        };
        if code.is_dead() {
            return;
        }
        self.mark_expression_line(opening.block, opening.line, code);
        code.nop();
    }

    fn opens_spliced_lambda_frame(&self, stmts: &[u32]) -> bool {
        opens_spliced_lambda_frame(self.ir, stmts)
    }

    /// Effects of a `Unit` coercion, then the `Unit` value that follows the lambda's closing brace.
    fn unit_after_lambda_effects(
        &self,
        outer_stmts: &[u32],
        value: u32,
    ) -> Option<(Vec<u32>, u32)> {
        if !self.opens_spliced_lambda_frame(outer_stmts) {
            return None;
        }
        if matches!(self.ir.expr(value), IrExpr::UnitInstance) {
            return Some((Vec::new(), value));
        }
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
                // The brace belongs to the lambda supplied by the inner frame's caller. It is a
                // raw source line there, but maps through the containing frame when that caller is
                // itself copied into another inline call.
                if self
                    .ir
                    .inline_copy_provenance(block)
                    .is_some_and(|frames| frames.len() > 1)
                {
                    self.mark_expression_line(block, line, code);
                } else {
                    code.mark_line(line);
                }
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
        block: u32,
        stmts: Vec<u32>,
        value: Option<u32>,
        terminal_target: Option<Label>,
        entry: InlineBodyEntry,
        close_statement_lambda: bool,
        code: &mut CodeBuilder,
    ) -> EmittedBlock {
        let enclosing_statement_line = self.statement_line;
        self.block_depth += 1;
        let mut dead = false;
        let last_statement = stmts.len().checked_sub(1);
        let mut lambda_unit_after_effects = close_statement_lambda
            .then(|| value.and_then(|value| self.unit_after_lambda_effects(&stmts, value)))
            .flatten();
        let closes_lambda_before_locals = lambda_unit_after_effects.is_some();
        let opens_lambda_frame = value.is_some_and(|value| {
            matches!(self.ir.expr(value), IrExpr::GetValue(_))
                && stmts.iter().any(|&statement| {
                    matches!(
                        self.ir.debug_local_provenance(statement),
                        Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                    )
                })
        });
        let InlineBodyEntry {
            mut normalize_at_lambda_frame,
            stack_height: inline_stack_height,
            within_inline_lambda,
            opening,
            copied_function,
            discarded_frame_result,
        } = entry;
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
        let optimized_postfix = self.emit_postfix_iinc(block, true, code);
        let mut bound_operands = 0;
        for (index, statement) in stmts.iter().copied().enumerate() {
            if optimized_postfix {
                break;
            }
            if bound_operands > 0 {
                bound_operands -= 1;
                continue;
            }
            self.open_external_inline_body(opening, statement, code);
            self.enter_copied_function_frame(copied_function, statement, code);
            // FixStack replaces the opening marker with the saved caller stack. Keep that
            // generated store inside the lambda frame's mapped line interval.
            self.mark_statement_line(statement, code);
            if normalize_at_lambda_frame
                && matches!(
                    self.ir.debug_local_provenance(statement),
                    Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
                )
            {
                reserved_inline_stack = Some(self.open_inline_stack_at_lambda_frame(
                    inline_stack_height,
                    within_inline_lambda,
                    code,
                ));
                normalize_at_lambda_frame = false;
            }
            let base = code.stack_height();
            self.terminal_statement_target = (value.is_none() && Some(index) == last_statement)
                .then_some(terminal_target)
                .flatten();
            bound_operands = self.emit_invocation_operand_bindings(&stmts[index..], code);
            if bound_operands > 0 {
                bound_operands -= 1;
                code.set_stack(base.max(0) as u16);
                continue;
            }
            self.emit(statement, code);
            if self.discarding_diverges(statement) {
                dead = true;
                break;
            }
            code.set_stack(base.max(0) as u16);
        }
        let mut discard_after_frame_close = None;
        let mut retains_postfix_temporary = false;
        let mut close_lambda_before_locals = false;
        let mut emit_after_frame_close = None;
        let mut discard_after_frame = None;
        let mut close_discarded_provider_frame = None;
        if !dead && !optimized_postfix {
            if let Some(value) = value {
                self.mark_statement_line(value, code);
                if let Some(result) = discarded_frame_result {
                    let expression = result.expression();
                    close_discarded_provider_frame = (!result.is_owned()).then_some(expression);
                    discard_after_frame = self.emit_discarded_frame_result(value, expression, code);
                } else if closes_lambda_before_locals {
                    let (effects, unit) = lambda_unit_after_effects
                        .take()
                        .expect("the Unit lambda-frame shape was identified above");
                    for effect in effects {
                        let base = code.stack_height();
                        self.emit(effect, code);
                        code.set_stack(base.max(0) as u16);
                    }
                    close_lambda_before_locals = true;
                    emit_after_frame_close = Some(unit);
                } else if self.ir.postfix_updates.contains(&block) {
                    self.emit_value(value, code);
                    discard_after_frame_close = Some(value);
                    retains_postfix_temporary = true;
                } else if opens_lambda_frame {
                    self.emit_value(value, code);
                    discard_after_frame_close = Some(value);
                } else if close_statement_lambda
                    && self.opens_spliced_lambda_frame(&stmts)
                    && self.leaves_lambda_result(value)
                {
                    // kotlinc leaves an inlined lambda's result on the stack until the inline
                    // call has returned to its own line; the statement discards it there.
                    let before = code.stack_height();
                    self.emit_value(value, code);
                    self.mark_lambda_result_return(value, code);
                    let words = code.stack_height() - before;
                    discard_after_frame =
                        (!code.is_dead() && matches!(words, 1 | 2)).then_some(words as u16);
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
            retains_postfix_temporary,
            reserved_inline_stack,
            close_lambda_before_locals,
            emit_after_frame_close,
            discard_after_frame,
            close_discarded_provider_frame,
        }
    }

    /// Open the bracket FixStack saves the caller's operand stack across, at the inline lambda's
    /// frame marker, after the call's operands are stored.
    ///
    /// Where the saved words go follows where kotlinc's FixStack runs over the bracket. In the
    /// method's own body it runs over the finished method, so the words go above every local the
    /// method uses (`LocalVariablesManager`'s initial `maxLocals`): a plain opening marker. Within
    /// an enclosing inline lambda's body (`within_inline_lambda`, read before this expansion enters
    /// its own frame markers) it ran over that lambda's node before the node was copied here
    /// (`MethodInliner.preprocessNodeBeforeInline`), which claimed them ahead of the lambda's own
    /// frame marker: the words are reserved before that marker takes its slot.
    /// `inline_stack_height` is captured at the expansion boundary: declarations before the delayed
    /// marker are stack-neutral, while their nested emission may make the builder's linear counter
    /// drift from that verified boundary. The emitted marker preserves the allocation boundary until
    /// the finished method is normalized; the reservations themselves keep subsequent frame-map
    /// entries above it.
    fn open_inline_stack_at_lambda_frame(
        &mut self,
        inline_stack_height: i32,
        within_inline_lambda: bool,
        code: &mut CodeBuilder,
    ) -> Vec<TempSlot> {
        if !within_inline_lambda {
            open_inline_stack(code);
            return Vec::new();
        }
        let Ok(words) = u16::try_from(inline_stack_height) else {
            self.run.set_emit_error(
                "the operand stack at an inline lambda boundary exceeds the JVM limit".to_string(),
            );
            code.inline_call_marker_with_reserved_locals();
            code.set_stack_height(0);
            return Vec::new();
        };
        let mut reserved = Vec::with_capacity(usize::from(words));
        for _ in 0..words {
            reserved.push(self.frame.enter_temp(TempRole::InlineStackSpill, Ty::Int));
        }
        code.inline_call_marker_with_reserved_locals();
        code.set_stack_height(0);
        reserved
    }

    /// Whether code emitted now belongs to an inline lambda's body: kotlinc builds that body as a
    /// node of its own and normalizes its stack before copying it into the method.
    fn within_inline_lambda_frame(&self) -> bool {
        self.frame.open_inline_frames().any(|marker| {
            matches!(
                self.ir.debug_local_provenance(marker),
                Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
            )
        })
    }

    /// The exact block whose result adaptation must remain inside an inline-stack bracket.
    pub(super) fn inline_stack_coercion_block(
        &self,
        expression: u32,
        stack_height: i32,
    ) -> Option<u32> {
        inline_stack_coercion_owner(self.ir, expression, stack_height)
    }

    fn close_or_defer_inline_stack(
        &mut self,
        block: u32,
        saved: i32,
        reserved: &mut Option<Vec<TempSlot>>,
        code: &mut CodeBuilder,
    ) {
        if let Some(pending) = self.inline_coercions.pending_for(block) {
            pending.saved = Some(saved);
            pending.reserved = reserved.take();
        } else {
            close_inline_stack(saved, code);
        }
    }

    fn release_reserved_inline_stack(&mut self, reserved: Option<Vec<TempSlot>>) {
        for slot in reserved.into_iter().flatten().rev() {
            self.frame.leave_temp(slot);
        }
    }

    /// Close the inline stack bracket that an enclosing coercion kept open so its representation
    /// adaptation remains inside FixStack's result boundary.
    pub(super) fn finish_pending_inline_stack_coercion(&mut self, code: &mut CodeBuilder) -> bool {
        let Some(pending) = self.inline_coercions.take_pending() else {
            return false;
        };
        let Some(saved) = pending.saved else {
            self.inline_coercions.restore_pending(Some(pending));
            return false;
        };
        close_inline_stack(saved, code);
        self.release_reserved_inline_stack(pending.reserved);
        if pending.closes_spliced_frame {
            self.close_spliced_lambda_frame(pending.block, false, code);
        }
        true
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

/// Follow effect-free value wrappers to the copied block that owns the physical stack boundary.
///
/// Lowering retains these wrappers when a copied inline result crosses a coercion. Since they own
/// no statements or locals, adapting outside them would also move the coercion outside the copied
/// stack bracket.
fn inline_stack_coercion_owner(
    ir: &crate::ir::IrFile,
    mut expression: u32,
    stack_height: i32,
) -> Option<u32> {
    if stack_height == 0 {
        return None;
    }
    loop {
        let IrExpr::Block { stmts, value } = ir.expr(expression) else {
            return None;
        };
        let has_lambda_frame = opens_spliced_lambda_frame(ir, stmts);
        if ir.external_inline_expansions.contains(&expression)
            || (ir.is_inline_copy(expression) && has_lambda_frame)
        {
            return Some(expression);
        }
        match (stmts.is_empty(), value) {
            (true, Some(value)) => expression = *value,
            _ => return None,
        }
    }
}

fn opens_spliced_lambda_frame(ir: &crate::ir::IrFile, stmts: &[u32]) -> bool {
    stmts.iter().any(|&statement| {
        matches!(
            ir.debug_local_provenance(statement),
            Some(IrDebugLocalProvenance::LambdaFrameMarker { .. })
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_stack_owner_is_found_through_only_empty_value_wrappers() {
        let mut ir = crate::ir::IrFile::default();
        let marker = ir.add_expr(IrExpr::InlineFrameMarker);
        ir.set_debug_local_provenance(
            marker,
            IrDebugLocalProvenance::LambdaFrameMarker {
                implementation: 0,
                depth: 1,
            },
        );
        let value = ir.add_expr(IrExpr::UnitInstance);
        let owner = ir.add_expr(IrExpr::Block {
            stmts: vec![marker],
            value: Some(value),
        });
        ir.mark_inline_copy(owner);

        assert_eq!(inline_stack_coercion_owner(&ir, owner, 1), Some(owner));
        assert_eq!(inline_stack_coercion_owner(&ir, owner, 0), None);

        let first_wrapper = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(owner),
        });
        let second_wrapper = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(first_wrapper),
        });
        assert_eq!(
            inline_stack_coercion_owner(&ir, second_wrapper, 1),
            Some(owner)
        );

        let effect = ir.add_expr(IrExpr::UnitInstance);
        let effectful_wrapper = ir.add_expr(IrExpr::Block {
            stmts: vec![effect],
            value: Some(owner),
        });
        assert_eq!(inline_stack_coercion_owner(&ir, effectful_wrapper, 1), None);
    }
}

/// Open a bracket FixStack saves the caller's operand stack across. Its analysis clears the stack at
/// the opening marker, so the expansion inside counts its operands from an empty stack, the way
/// kotlinc's inliner sees the node it copies in: a bracket nested inside saves only what is pushed
/// after this one.
fn open_inline_stack(code: &mut CodeBuilder) {
    code.inline_call_marker(true);
    code.set_stack_height(0);
}

/// Close the bracket [`open_inline_stack`] opened when the caller had `saved` words on the stack:
/// FixStack puts them back under the expansion's result.
fn close_inline_stack(saved: i32, code: &mut CodeBuilder) {
    let result = code.stack_height();
    code.inline_call_marker(false);
    code.set_stack((saved + result).max(0) as u16);
}

/// An open lexical slot scope; see [`Emitter::open_slot_scope`].
pub(super) struct SlotScope {
    slots: HashMap<u32, (u16, Ty)>,
    frame: Mark,
}

/// How a block that may be an external inline expansion enters its inlined body.
#[derive(Clone, Copy, Default)]
pub(super) struct InlineBodyEntry {
    /// Preserve the caller stack at the lambda frame marker rather than at the block boundary.
    pub(super) normalize_at_lambda_frame: bool,
    /// The caller's operand stack height when the block began.
    pub(super) stack_height: i32,
    /// Whether the block is emitted inside an enclosing inline lambda's body (see
    /// [`Emitter::open_inline_stack_at_lambda_frame`]).
    pub(super) within_inline_lambda: bool,
    opening: Option<InlineBodyOpening>,
    /// The same-module copied function whose caller state is suspended at its frame marker.
    copied_function: Option<u32>,
    /// A discarded same-module inline result emitted inside its copied frame and popped only after
    /// that frame's locals end.
    discarded_frame_result: Option<CopiedFrameTail>,
}

/// The first frame marker of an external inline expansion and the call line its opening `nop`
/// carries.
#[derive(Clone, Copy)]
struct InlineBodyOpening {
    block: u32,
    marker: u32,
    line: u32,
}

#[derive(Clone, Copy)]
enum CopiedFrameResult {
    Unit,
    Invocation {
        invocation: u32,
        ret: Ty,
        adapts_before_lambda_close: bool,
    },
    /// A nested provider plan still owns the adaptation; the copied frame only delays it until
    /// its own locals have ended.
    ProviderInvocation {
        invocation: u32,
        ret: Ty,
        caller_line: u32,
    },
    /// A same-module copied body physically produces its declaration-erased result inside the
    /// frame; the caller narrows or unboxes it only after the frame's debug locals end.
    Coercion {
        expression: u32,
        arg: u32,
        type_operand: Ty,
        physical_arg: Option<Ty>,
    },
}

impl CopiedFrameResult {
    fn is_provider(self) -> bool {
        matches!(self, Self::ProviderInvocation { .. })
    }
}

#[derive(Clone, Copy)]
struct PendingPreCloseCopiedResult {
    invocation: u32,
    ret: Ty,
}

/// Deferred representation adaptations owned by copied inline block emission.
///
/// The erased producer runs while the copied frame is open, while narrowing and the closing
/// FixStack marker wait until that frame's debug locals have closed.
#[derive(Default)]
pub(super) struct InlineCoercionState {
    /// An erased copied result whose safe-call selector owns adaptation, but whose trailing
    /// spliced lambda must perform it before closing its locals and inline frame.
    pre_close: Option<PendingPreCloseCopiedResult>,
    deferred: HashMap<crate::ir::ExprId, Option<Ty>>,
    pending: Option<PendingInlineStackCoercion>,
}

impl InlineCoercionState {
    pub(super) fn begin_deferred(&mut self, expression: crate::ir::ExprId) -> bool {
        if self.deferred.contains_key(&expression) {
            return false;
        }
        self.deferred.insert(expression, None);
        true
    }

    pub(super) fn is_deferred(&self, expression: crate::ir::ExprId) -> bool {
        self.deferred.contains_key(&expression)
    }

    pub(super) fn record_deferred_result(&mut self, expression: crate::ir::ExprId, physical: Ty) {
        *self
            .deferred
            .get_mut(&expression)
            .expect("a deferred inline coercion remains registered while it emits") =
            Some(physical);
    }

    pub(super) fn finish_deferred(&mut self, expression: crate::ir::ExprId) -> Option<Ty> {
        self.deferred.remove(&expression).flatten()
    }

    pub(super) fn replace_pending(
        &mut self,
        block: Option<u32>,
    ) -> Option<PendingInlineStackCoercion> {
        std::mem::replace(
            &mut self.pending,
            block.map(PendingInlineStackCoercion::new),
        )
    }

    pub(super) fn restore_pending(&mut self, pending: Option<PendingInlineStackCoercion>) {
        self.pending = pending;
    }

    fn pending_for(&mut self, block: u32) -> Option<&mut PendingInlineStackCoercion> {
        self.pending
            .as_mut()
            .filter(|pending| pending.block == block)
    }

    fn take_pending(&mut self) -> Option<PendingInlineStackCoercion> {
        self.pending.take()
    }
}

pub(super) struct PendingInlineStackCoercion {
    pub(super) block: u32,
    saved: Option<i32>,
    reserved: Option<Vec<TempSlot>>,
    closes_spliced_frame: bool,
}

impl PendingInlineStackCoercion {
    fn new(block: u32) -> Self {
        Self {
            block,
            saved: None,
            reserved: None,
            closes_spliced_frame: false,
        }
    }
}

#[derive(Clone, Copy)]
enum CopiedFrameTail {
    Owned(u32),
    Provider(u32),
}

impl CopiedFrameTail {
    fn expression(self) -> u32 {
        match self {
            Self::Owned(expression) | Self::Provider(expression) => expression,
        }
    }

    fn is_owned(self) -> bool {
        matches!(self, Self::Owned(_))
    }
}

pub(super) struct EmittedBlock {
    pub(super) discard_after_close: Option<u32>,
    /// A non-`iinc` postfix statement keeps its old-value temporary through block close.
    pub(super) retains_postfix_temporary: bool,
    pub(super) reserved_inline_stack: Option<Vec<TempSlot>>,
    /// A statement-position spliced lambda whose `Unit` value follows its effects closes on the
    /// brace while its frame locals are still live, before the surrounding lexical scope ends.
    pub(super) close_lambda_before_locals: bool,
    /// The trailing `Unit` is emitted after the lambda's marker/local range closes. A retained
    /// mapped inline-return line becomes its own `nop`; an ordinary discarded `Unit` emits none.
    pub(super) emit_after_frame_close: Option<u32>,
    /// Physical JVM words left by a discarded copied result and popped after its frame ends.
    pub(super) discard_after_frame: Option<u16>,
    /// A provider-owned tail closes on its declaration line while copied-frame locals remain live;
    /// the delayed pop follows only after those locals end.
    close_discarded_provider_frame: Option<u32>,
}

fn discard_stack_words(words: u16, code: &mut CodeBuilder) {
    match words {
        1 => code.pop(),
        2 => code.pop2(),
        _ => unreachable!("a JVM value occupies one or two words"),
    }
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

/// The caller state a copied inline-function body suspends: the line in effect at the call,
/// whether the call stands in a condition, and the enclosing call's operand binding it may be.
struct CopiedFunctionCaller {
    line: Option<u16>,
    inside_condition: bool,
    operand_binding: Option<u32>,
}
