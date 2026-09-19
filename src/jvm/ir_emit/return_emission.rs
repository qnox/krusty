//! Return emission, including the control transfer through active `finally` blocks.

use crate::ir::ExprId;
use crate::jvm::classfile::CodeBuilder;

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
        // This copy of `finalizer` lies in the middle of its own try's protected region. Close the
        // open segment ahead of it; the caller reopens once the whole transfer is emitted.
        self.close_finally_segment(finalizer, code);
        self.emit(finalizer, code);
        let survives =
            !self.discarding_diverges(finalizer) && self.emit_transfer_finalizers(floor, code);
        self.return_finalizers.push(finalizer);
        survives
    }

    /// Every active `finally`, for a `return` — which leaves them all.
    fn emit_return_finalizers(&mut self, code: &mut CodeBuilder) -> bool {
        self.emit_transfer_finalizers(0, code)
    }

    pub(super) fn emit_return_node(
        &mut self,
        returned: ExprId,
        value: Option<ExprId>,
        code: &mut CodeBuilder,
    ) {
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
        self.emit_value_as(value, &ret, code);
        // `return <diverging>` has already transferred control and must not grow dead bytecode.
        if self.diverges(value) {
            return;
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
        let slot = self.next_slot;
        self.next_slot += words;
        store(ret, slot, code);
        // The pending return value is initialized before every active `finally` and remains live
        // until the finalizer chain either completes or overrides the transfer. Any branch or
        // handler frame created while emitting a finalizer must therefore carry this slot. Merely
        // reserving `next_slot` leaves it as `top`, which makes a later reload unverifiable after a
        // branchy finalizer such as `null?.toString()`. It is a BACKEND temporary, not a semantic
        // local: `slots` is keyed by real value ids, so parking it there under a reserved numeric
        // range could overwrite a value with that id, be filtered out as unassigned, and then be
        // removed when the transfer finishes.
        let parked = self.lease_temporary(slot, ret);
        let survives = self.emit_return_finalizers(code);
        self.release_temporary(parked);
        if survives {
            load(ret, slot, code);
            debug_lines::mark_return(self.ir, returned, code);
            emit_return(ret, code);
        }
        self.reopen_finally_segments(code);
    }
}
