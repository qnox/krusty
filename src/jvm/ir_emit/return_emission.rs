//! Return emission, including the control transfer through active `finally` blocks.

use crate::ir::ExprId;
use crate::jvm::classfile::CodeBuilder;

use super::{debug_lines, emit_return, load, slot_words, store, Emitter};

impl Emitter<'_> {
    /// Emit every active `finally` from inner to outer before a `return`. The active entry is popped
    /// while its body emits so a return *inside* that finally overrides the pending transfer without
    /// recursively entering the same finally again. Returns whether the original transfer survives.
    fn emit_return_finalizers(&mut self, code: &mut CodeBuilder) -> bool {
        let Some(finalizer) = self.return_finalizers.pop() else {
            return true;
        };
        self.emit(finalizer, code);
        let survives = !self.discarding_diverges(finalizer) && self.emit_return_finalizers(code);
        self.return_finalizers.push(finalizer);
        survives
    }

    pub(super) fn emit_return_node(
        &mut self,
        returned: ExprId,
        value: Option<ExprId>,
        code: &mut CodeBuilder,
    ) {
        let Some(value) = value else {
            if self.emit_return_finalizers(code) {
                code.ret_void();
            }
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
        // branchy finalizer such as `null?.toString()`.
        let return_key = 5_000_000 + slot as u32;
        self.slots.insert(return_key, (slot, ret));
        let survives = self.emit_return_finalizers(code);
        self.slots.remove(&return_key);
        if survives {
            load(ret, slot, code);
            debug_lines::mark_return(self.ir, returned, code);
            emit_return(ret, code);
        }
    }
}
