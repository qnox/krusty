//! `try` / `catch` / `finally` emission, and the protected regions it maintains.
//!
//! A `try`'s protected region covers everything lexically inside it EXCEPT the inlined copies of its
//! own `finally`. Kotlin runs that finalizer whenever control leaves the `try`, so a copy is emitted
//! on the normal path, in each catch, in the catch-all handler, and at every `return`, `break` and
//! `continue` that leaves it. Each copy has to fall outside the region it belongs to, or an
//! exception raised while the finalizer runs would re-enter its own handler and run it twice.
//!
//! The region is therefore an accumulator rather than one range: a control transfer closes the open
//! segment ahead of the copy it is about to run and opens a fresh one once the transfer is emitted.

use crate::jvm::classfile::{CodeBuilder, Label};
use crate::types::Ty;

use super::{
    debug_lines, ir_ty_to_jvm, load, local_variable_desc, slot_words, store, Emitter, VerifType,
};

/// One `try`'s protected region while it is being emitted.
///
/// The region covers everything lexically inside the `try` EXCEPT the inlined copies of that try's
/// own `finally`. A `return` out of the body inlines such a copy in the middle of the region, so the
/// open segment is closed ahead of it and a fresh one opened once the whole `return` has been
/// emitted — which is usually empty, because a `return` ends its enclosing scope.
///
/// Nested `try`s are unaffected by each other: an inner finalizer copy is ordinary code as far as an
/// outer region is concerned, and stays inside it.
pub(super) struct FinallyRegion {
    /// The exact finalizer this region belongs to; regions are matched by IR identity, never by
    /// nesting depth, so a `return` closes the region of the finalizer it is actually running.
    finalizer: u32,
    /// Segments closed so far.
    segments: Vec<(Label, Label)>,
    /// Start of the segment currently open, if one is.
    open: Option<Label>,
}

impl Emitter<'_> {
    /// `try { body } catch (v: E) { … } …` (no `finally`). The body value (and each catch value) is
    /// stored into a result temp, then loaded at the merge — mirroring kotlinc. The protected region
    /// `[start, end)` covers the body+store; each catch is an exception-table handler whose frame has
    /// the caught exception on the stack and the pre-`try` locals (the result temp/catch var read as
    /// `top` there, since an exception may occur before they are assigned).
    /// The region of an ACTIVE finalizer. Presence is an invariant, not a condition: `emit_try`
    /// pushes the region and the finalizer together and takes the region back only after the last
    /// catch body has popped it, so every finalizer reachable by a transfer has one.
    fn finally_region(&mut self, finalizer: u32) -> usize {
        self.finally_regions
            .iter()
            .rposition(|region| region.finalizer == finalizer)
            .expect("an active finalizer owns a protected region")
    }

    /// Start a protected segment for `finalizer` at the current offset. Whether one is already open
    /// is the only conditional part: a transfer that runs no finalizer leaves the segment as it is.
    pub(super) fn open_finally_segment(&mut self, finalizer: u32, code: &mut CodeBuilder) {
        let index = self.finally_region(finalizer);
        if self.finally_regions[index].open.is_some() {
            return;
        }
        let label = code.new_label();
        self.finally_regions[index].open = Some(label);
        self.bind(label, code);
    }

    /// End `finalizer`'s open protected segment at the current offset. A segment that turns out to
    /// be empty is dropped when the table is resolved: an empty range protects nothing, and kotlinc
    /// emits no entry for one.
    pub(super) fn close_finally_segment(&mut self, finalizer: u32, code: &mut CodeBuilder) {
        let index = self.finally_region(finalizer);
        let Some(start) = self.finally_regions[index].open.take() else {
            return;
        };
        let label = code.new_label();
        self.finally_regions[index].segments.push((start, label));
        self.bind(label, code);
    }

    /// Reopen every active region a control transfer closed. Called once the transfer is fully
    /// emitted, so the copies of every finalizer it ran are outside their own regions.
    pub(super) fn reopen_finally_segments(&mut self, code: &mut CodeBuilder) {
        for finalizer in self.return_finalizers.clone() {
            self.open_finally_segment(finalizer, code);
        }
    }

    /// The segments of `finalizer`'s region, closed at the current offset.
    fn take_finally_region(
        &mut self,
        finalizer: u32,
        code: &mut CodeBuilder,
    ) -> Vec<(Label, Label)> {
        self.close_finally_segment(finalizer, code);
        let index = self.finally_region(finalizer);
        self.finally_regions.remove(index).segments
    }

    pub(super) fn emit_try(
        &mut self,
        expression: u32,
        body: u32,
        catches: &[crate::ir::IrCatch],
        finally: Option<u32>,
        result: &Ty,
        code: &mut CodeBuilder,
    ) {
        let rt = ir_ty_to_jvm(result);
        let is_stmt = matches!(rt, Ty::Unit | Ty::Nothing);
        let result_slot = if is_stmt {
            None
        } else {
            let s = self.next_slot;
            self.next_slot += slot_words(rt);
            Some(s)
        };
        const RESULT_KEY: u32 = 3_000_000;
        // A `finally` that diverges (`finally { throw }`) never falls through to `after`.
        let fin_diverges = finally.is_some_and(|f| self.discarding_diverges(f));

        let start = code.new_label();
        let end = code.new_label();
        let after = code.new_label();

        self.bind(start, code);
        // kotlinc opens every protected region with a `nop` carrying the `try` keyword's line, so the
        // region starts at an instruction of its own rather than sharing the body's first one. The
        // exception table's `from` is that `nop`.
        debug_lines::mark_expression_start(self.ir, expression, code);
        code.nop();
        let body_diverges = if is_stmt {
            self.discarding_diverges(body)
        } else {
            self.diverges(body)
        };
        if let Some(finalizer) = finally {
            self.finally_regions.push(FinallyRegion {
                finalizer,
                segments: Vec::new(),
                open: Some(start),
            });
            self.return_finalizers.push(finalizer);
        }
        if is_stmt || body_diverges {
            // Statement, or a diverging body (`throw`/`return`): no value reaches the result temp.
            self.emit(body, code);
        } else {
            self.emit_value(body, code);
            store(rt, result_slot.unwrap(), code);
        }
        if let Some(finalizer) = finally {
            self.return_finalizers.pop();
            // The normal-path copy of this finalizer is emitted next and must lie outside its own
            // region.
            self.close_finally_segment(finalizer, code);
        }
        self.bind(end, code);
        let mut after_reachable = false;
        if !body_diverges {
            if let Some(f) = finally {
                self.emit(f, code);
            } // `finally` inlined on the normal path
            if !fin_diverges {
                code.goto(after);
                after_reachable = true;
            }
        }

        // The `finally` catch-all must protect the body and each catch BODY, but NOT the inlined finally
        // code (normal-path, per-catch, or its own) — otherwise an exception thrown inside an inlined
        // finally re-enters the handler and the finally runs twice. Collect each catch body's range
        // (`[cbody_start, cbody_end)`, ending before that catch's inlined finally).
        for c in catches {
            let handler = code.new_label();
            // A handler is entered over the exception edge, not by a branch — and a diverging `try`
            // body leaves the stream dead exactly here, so binding must revive on the range it guards
            // rather than on an incoming branch.
            code.bind_handler(handler, &[(start, end)]);
            let exc_internal = crate::jvm::names::classfile_internal_name(&c.exc_internal.render());
            let exc_ci = self.cw.class_ref(&exc_internal);
            // Handler entry: the exception is the sole stack value; locals are the pre-`try` state.
            self.frame(handler, vec![VerifType::Object(exc_ci)], code);
            let exc_ty = Ty::obj(&exc_internal);
            let cslot = self.next_slot;
            self.next_slot += 1;
            self.slots.insert(c.var, (cslot, exc_ty));
            store(exc_ty, cslot, code);
            let local_start =
                (code.bytes.len() <= u16::MAX as usize).then_some(code.bytes.len() as u16);
            let cbody_start = code.new_label();
            self.bind(cbody_start, code);
            if let Some(finalizer) = finally {
                self.open_finally_segment(finalizer, code);
            }
            let cbody_diverges = if is_stmt {
                self.discarding_diverges(c.body)
            } else {
                self.diverges(c.body)
            };
            if let Some(finalizer) = finally {
                self.return_finalizers.push(finalizer);
            }
            if is_stmt || cbody_diverges {
                self.emit(c.body, code);
            } else {
                self.emit_value(c.body, code);
                store(rt, result_slot.unwrap(), code);
            }
            if finally.is_some() {
                self.return_finalizers.pop();
            }
            self.slots.remove(&c.var);
            // The catch body is protected by the finally handler (a throw in a catch runs the finally),
            // but the catch's own inlined finally (below) is not.
            let cbody_end = code.new_label();
            self.bind(cbody_end, code);
            if let Some(finalizer) = finally {
                // As on the normal path: this catch's own inlined copy follows and stays outside.
                self.close_finally_segment(finalizer, code);
            }
            if self.record_locals {
                if let (Some(name), Some(start_pc)) = (c.name.as_deref(), local_start) {
                    let end_pc = code.bytes.len().min(u16::MAX as usize) as u16;
                    code.add_local_entry(
                        start_pc,
                        Some(end_pc.saturating_sub(start_pc)),
                        cslot,
                        name,
                        &local_variable_desc(exc_ty),
                    );
                }
            }
            if !cbody_diverges {
                if let Some(f) = finally {
                    self.emit(f, code);
                } // `finally` inlined after the catch
                if !fin_diverges {
                    code.goto(after);
                    after_reachable = true;
                }
            }
            code.add_exception(start, end, handler, exc_ci);
        }

        // `finally` catch-all: any exception not handled above (in the body or a catch body) runs the
        // `finally` then re-throws. It protects only the body + catch bodies (`fin_ranges`), NOT the
        // inlined finally code — which lies past those ranges, so it doesn't re-catch itself.
        if let Some(f) = finally {
            let mut fin_ranges = self.take_finally_region(f, code);
            let fin_handler = code.new_label();
            // Exception edge — see the `catch` handler above; this one guards the body and every
            // catch body (`fin_ranges`), which are complete by now.
            code.bind_handler(fin_handler, &fin_ranges);
            let thr_ci = self.cw.class_ref("java/lang/Throwable");
            self.frame(fin_handler, vec![VerifType::Object(thr_ci)], code);
            let thr_ty = Ty::obj("java/lang/Throwable");
            let tslot = self.free_exception_slots.pop().unwrap_or_else(|| {
                let leased = self.next_slot;
                self.next_slot += 1;
                leased
            });
            // The handler's entry belongs to the finalizer copy it introduces, not to the `finally`
            // keyword — mark it before the store so both copies open on the same line.
            debug_lines::mark_block_entry(self.ir, f, code);
            store(thr_ty, tslot, code);
            // kotlinc protects the handler's own entry — everything ahead of the copy of the
            // finalizer it introduces — with a range of its own.
            let handler_protected_end = code.new_label();
            self.bind(handler_protected_end, code);
            fin_ranges.push((fin_handler, handler_protected_end));
            // The caught exception is LIVE in `tslot` across the whole inlined `finally` (it is re-raised
            // after it). Register it so any StackMapTable frame recorded WHILE emitting the finally —
            // e.g. a `finally` that itself contains a `try`/`catch` — lists `tslot` as an initialized
            // local; otherwise the trailing `aload tslot; athrow` reads a slot the verifier sees as `top`.
            // Keyed by the slot number (unique, and disjoint from small value indices) so nested catch-all
            // handlers each register their own live exception.
            let thr_key = 4_000_000 + tslot as u32;
            self.slots.insert(thr_key, (tslot, thr_ty));
            self.emit(f, code);
            self.slots.remove(&thr_key);
            // Re-raise the caught exception after the `finally` — unless the `finally` itself transfers
            // control (`finally { return … }` / `finally { throw … }`), in which case the rethrow is
            // unreachable and emitting it would leave a dead instruction without a stackmap frame.
            if !fin_diverges {
                load(thr_ty, tslot, code);
                code.athrow();
            }
            // The parked exception is dead past the rethrow, so the slot returns to the pool for an
            // enclosing handler to lease.
            self.free_exception_slots.push(tslot);
            // `catch_type` 0 = catch-all (any throwable), matching kotlinc's `finally` table entry.
            for (rs, re) in fin_ranges {
                code.add_exception(rs, re, fin_handler, 0);
            }
        }

        if after_reachable {
            if let Some(slot) = result_slot {
                self.slots.insert(RESULT_KEY, (slot, rt));
            }
            self.frame(after, vec![], code);
            self.bind(after, code);
            if let Some(slot) = result_slot {
                load(rt, slot, code);
                self.slots.remove(&RESULT_KEY);
            }
        } else {
            // Every path diverges — `after` is dead; bind it so any stray reference resolves, but emit
            // no frame (nothing reaches it) and leave no value (the `try` is `Nothing`-typed).
            self.bind(after, code);
        }
    }

    /// The loop `label` names, as `(continue target, break target, active-finalizer depth on
    /// entry)`. A transfer to it runs every finalizer pushed above that depth. `None` → the
    /// innermost loop; `Some(l)` → the nearest enclosing loop carrying `l@`. Falls back to the
    /// innermost if the label isn't found (a compilable program always has the labeled loop in
    /// scope).
    pub(super) fn loop_transfer_target(&self, label: &Option<String>) -> (Label, Label, usize) {
        let entry = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|(_, _, sl, _)| sl.as_deref() == Some(l.as_str()))
                .or_else(|| self.loop_stack.last()),
            None => self.loop_stack.last(),
        };
        let (cont, end, _, depth) = entry.expect("break/continue outside loop");
        (*cont, *end, *depth)
    }

    /// Leave the loop `label` names, running every `finally` between here and it first.
    ///
    /// Kotlin executes a finalizer when control leaves its `try` by ANY route, not only by `return`.
    /// The finalizers above the loop's entry depth are exactly the ones this transfer leaves; an
    /// outer one belongs to a `try` the loop is nested in and must not run. A finalizer that itself
    /// transfers overrides the pending jump, which is why the emission reports whether it survives.
    pub(super) fn emit_loop_transfer(
        &mut self,
        label: &Option<String>,
        brk: bool,
        code: &mut CodeBuilder,
    ) {
        let (cont, end, depth) = self.loop_transfer_target(label);
        let target = if brk { end } else { cont };
        if self.return_finalizers.len() > depth {
            // kotlinc closes the protected region on a transfer that leaves a `try` with a `nop`,
            // so the region ends at an instruction of its own rather than at the first byte of the
            // finalizer copy that follows — the same boundary rule as the `nop` that opens it.
            code.nop();
        }
        let survives = self.emit_transfer_finalizers(depth, code);
        if survives {
            self.frame(target, vec![], code);
            code.goto(target);
        }
        self.reopen_finally_segments(code);
    }
}
