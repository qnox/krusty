//! Return emission, including the control transfer through active `finally` blocks.

use crate::ir::ExprId;
use crate::jvm::classfile::CodeBuilder;

use super::frame_map::TempRole;
use super::{debug_lines, emit_return, load, slot_words, store, Emitter};

impl Emitter<'_> {
    /// Emit the active `finally` bodies down to `floor`, inner to outer, before a control transfer.
    ///
    /// `floor` is how many finalizers the transfer does NOT leave: `0` for a `return`, which leaves
    /// the whole method, and a loop's entry depth for a `break`/`continue`, which leaves only the
    /// `try`s opened inside that loop. The active entry is popped while its body emits, so a
    /// transfer *inside* that finalizer overrides the pending one through this same operation
    /// instead of re-entering the finalizer. Returns whether the original transfer survives.
    pub(super) fn emit_transfer_finalizers(
        &mut self,
        floor: usize,
        code: &mut CodeBuilder,
    ) -> bool {
        if self.return_finalizers.len() <= floor {
            return true;
        }
        let finalizer = self
            .return_finalizers
            .pop()
            .expect("the stack is longer than the floor");
        // This copy of `finalizer` lies in the middle of its own try's protected region, and of
        // every region nested in it that the transfer has left. Close their open segments ahead of
        // it; the caller reopens once the whole transfer is emitted.
        self.close_left_regions(finalizer, code);
        self.emit(finalizer, code);
        let survives = if self.discarding_diverges(finalizer) {
            // kotlinc still inlines the outer finalizers behind a diverging one, as dead code its
            // optimizer then deletes. The regions those copies split stay split, at the offset the
            // deleted code collapses to: right behind this copy.
            if let Some(&outermost) = self.return_finalizers.get(floor) {
                self.close_left_regions(outermost, code);
            }
            false
        } else {
            self.emit_transfer_finalizers(floor, code)
        };
        self.return_finalizers.push(finalizer);
        survives
    }

    /// Every active `finally`, for a `return` — which leaves them all.
    fn emit_return_finalizers(&mut self, code: &mut CodeBuilder) -> bool {
        self.emit_transfer_finalizers(0, code)
    }

    /// A forwarded `Unit` suspend function's result under Kotlin 2.4.20: the callee's
    /// `COROUTINE_SUSPENDED` is returned as is (`dup; getCOROUTINE_SUSPENDED; if_acmpne; areturn`),
    /// and any other result is replaced by `Unit.INSTANCE`, which the caller's `return` then returns.
    fn emit_unit_result_of_forward(&mut self, code: &mut CodeBuilder) {
        let resumed = code.new_label();
        code.dup();
        let suspended = self.cw.methodref(
            "kotlin/coroutines/intrinsics/IntrinsicsKt",
            "getCOROUTINE_SUSPENDED",
            "()Ljava/lang/Object;",
        );
        code.invokestatic(suspended, 0, 1);
        code.if_acmpne(resumed);
        code.areturn();
        self.bind(resumed, code);
        code.pop();
        let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        code.getstatic(unit, 1);
    }

    pub(super) fn emit_return_node(
        &mut self,
        returned: ExprId,
        value: Option<ExprId>,
        code: &mut CodeBuilder,
    ) {
        // kotlinc's `setExtraLineNumberForVoidReturningFunction`: a body that falls off its end
        // marks its closing line before the return materializes its value. The `nop` kotlinc
        // writes there always shares that line with the value's load or the `return` and is
        // cleaned up, so it is not written.
        if let Some(line) = self.ir.fallthrough_return_line(returned) {
            code.mark_line(line);
        }
        let Some(value) = value else {
            // A void `return` emits nothing of its own, so with a finalizer active its line would
            // otherwise be claimed by the finalizer's first instruction. kotlinc anchors it on a
            // `nop` ahead of the transfer and restores it again at the physical return, exactly as
            // a value return's own expression and reload do.
            if !self.return_finalizers.is_empty() {
                debug_lines::mark_return(self.ir, returned, code);
                code.nop();
            }
            if self.emit_return_finalizers(code) {
                debug_lines::mark_return(self.ir, returned, code);
                code.ret_void();
            }
            self.reopen_finally_segments(code);
            return;
        };
        let ret = self.ret;
        if ret == crate::types::Ty::Unit {
            // A void function's `return <Unit expression>` evaluates that expression as a
            // statement: kotlinc leaves nothing of its value on the stack.
            self.emit_discarding(value, code);
        } else {
            self.emit_value_as(value, ret, code);
        }
        // `return <diverging>` has already transferred control and must not grow dead bytecode.
        if self.diverges(value) {
            return;
        }
        if self.unit_result_tail_forwards.contains(&returned) {
            self.emit_unit_result_of_forward(code);
        }
        let words = slot_words(ret);
        if self.return_finalizers.is_empty() || words == 0 {
            if self.emit_return_finalizers(code) {
                debug_lines::mark_return(self.ir, returned, code);
                emit_return(ret, code);
            }
            self.reopen_finally_segments(code);
            return;
        }
        // Kotlin evaluates the return expression before `finally`. Spill that value so arbitrary
        // branchy finalizers run on an empty operand stack, then reload it only if none overrides.
        // kotlinc's `generateFinallyBlocksIfNeeded` enters the spill as a temporary HERE, above
        // every local in scope at the `return`, so the finalizer copies declare their locals above
        // it, and leaves it after the reload.
        //
        // The pending return value is initialized before every active `finally` and remains live
        // until the finalizer chain either completes or overrides the transfer, so any branch or
        // handler frame recorded while a finalizer is emitted must type it: the lease does that.
        let temp = self.frame.enter_temp(TempRole::ReturnValue, ret);
        let slot = temp.slot();
        store(ret, slot, code);
        let parked = self.lease_frame_temporary(temp, ret);
        let survives = self.emit_return_finalizers(code);
        if survives {
            load(ret, slot, code);
        }
        self.release_temporary(parked);
        if survives {
            debug_lines::mark_return(self.ir, returned, code);
            emit_return(ret, code);
        }
        self.reopen_finally_segments(code);
    }
}
