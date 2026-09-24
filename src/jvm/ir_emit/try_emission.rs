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

use super::{debug_lines, ir_ty_to_jvm, load, local_variable_desc, slot_words, store, Emitter};

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
        // A `try` with a `finally` reserves its two slots HERE, before anything inside it is
        // emitted, because that is where kotlinc reserves them: the return value a `return` out of
        // the `try` parks while the finalizer runs, then the exception the catch-all parks while
        // it runs. Every local an inlined copy of the finalizer declares sits above both.
        //
        // Reserving them where they are first USED put the first copy's locals underneath instead,
        // which moved the parked exception one slot up and showed as an extra `top` in every frame
        // recorded while the finalizer ran.
        //
        // Nested `try`s SHARE both, as kotlinc's do: only one return is ever in flight, and a
        // `try` inside the body runs its handler strictly before the enclosing one is entered, so
        // the enclosing slots are free for it. The parked-exception slot therefore stays in the
        // reuse pool while the body is emitted and is taken back out before the handler, where it
        // holds the exception across the whole inlined finalizer.
        let return_words = slot_words(self.ret);
        let return_spill = self
            .pending_return_spills
            .last()
            .copied()
            .flatten()
            .or_else(|| {
                (finally.is_some() && return_words > 0 && self.parks_a_returned_value(expression))
                    .then(|| {
                        let reserved = self.next_slot;
                        self.next_slot += return_words;
                        reserved
                    })
            });
        self.pending_return_spills.push(return_spill);
        let parked_slot = finally.is_some().then(|| {
            let reserved = self.free_exception_slots.pop().unwrap_or_else(|| {
                let fresh = self.next_slot;
                self.next_slot += 1;
                fresh
            });
            self.free_exception_slots.push(reserved);
            reserved
        });
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
        // Typed catches guard only the try BODY. When a transfer out of the body has inlined this
        // try's own finalizer, the body is split into segments around that copy just like the
        // catch-all region below. Snapshot the body-only segments now: catch bodies are appended to
        // the active finally region later, but must never be guarded by their sibling typed catches.
        let typed_catch_ranges = if let Some(finalizer) = finally {
            let index = self.finally_region(finalizer);
            self.finally_regions[index].segments.clone()
        } else {
            vec![(start, end)]
        };
        let mut after_reachable = false;
        if !body_diverges {
            if let Some(f) = finally {
                // The result has just been stored and is loaded again at `after`, so it is live
                // across the finalizer inlined here. A finalizer that records frames of its own — a
                // nested `try`, a `when`, a null-safe call — must type it in each of them, or the
                // merge at `after`, which does type it, is rejected as inconsistent. The lease ends
                // with this copy: the handler copies below are reached on edges that never stored it.
                let parked = result_slot.map(|slot| self.lease_temporary(slot, rt));
                self.emit(f, code);
                if let Some(parked) = parked {
                    self.release_temporary(parked);
                }
            } // `finally` inlined on the normal path
            if !fin_diverges {
                if let Some(f) = finally {
                    debug_lines::mark_block_exit(self.ir, f, code);
                }
                code.goto(after);
                after_reachable = true;
            }
        }

        // The `finally` catch-all must protect the body and each catch BODY, but NOT the inlined finally
        // code (normal-path, per-catch, or its own) — otherwise an exception thrown inside an inlined
        // finally re-enters the handler and the finally runs twice. Collect each catch body's range
        // (`[cbody_start, cbody_end)`, ending before that catch's inlined finally).
        // A catch body is a scope of its own: the slot reserved for a `return` out of the TRY is
        // live there — the caught exception now occupies the one beside it — so a `return` written
        // in a catch takes a slot of its own, as kotlinc's does.
        self.pending_return_spills.push(None);
        for (ordinal, c) in catches.iter().enumerate() {
            let handler = code.new_label();
            // A handler is entered over the exception edge, not by a branch — and a diverging `try`
            // body leaves the stream dead exactly here, so binding must revive on the range it guards
            // rather than on an incoming branch.
            code.bind_handler(handler, &typed_catch_ranges);
            let exc_internal = crate::jvm::names::classfile_internal_name(&c.exc_internal.render());
            let exc_ci = self.cw.class_ref(&exc_internal);
            // Handler entry: the exception is the sole stack value; locals are the pre-`try` state.
            let exc_ty = Ty::obj(&exc_internal);
            // A typed catch's parameter takes the slot the `finally` catch-all parks its own
            // exception in, which is what kotlinc emits: the two are never live at once — a catch
            // body runs because its type MATCHED, and the catch-all parks only while unwinding past
            // it — and the parked value is dead the moment the handler rethrows. Giving the
            // parameter a slot of its own instead pushed it above the reserved one and cost a wide
            // `astore` at every catch.
            let cslot = parked_slot.unwrap_or_else(|| {
                let fresh = self.next_slot;
                self.next_slot += 1;
                fresh
            });
            self.slots.insert(c.var, (cslot, exc_ty));
            // The `finally` guards this catch from its ENTRY, the store of the caught exception
            // included — kotlinc protects the handler's own entry the same way it protects the
            // catch-all's, and a throw between the exception edge and the body is still a throw out
            // of the `try` the finalizer belongs to.
            if let Some(finalizer) = finally {
                self.open_finally_segment(finalizer, code);
            }
            store(exc_ty, cslot, code);
            let local_start =
                (code.bytes.len() <= u16::MAX as usize).then_some(code.bytes.len() as u16);
            let cbody_start = code.new_label();
            self.bind(cbody_start, code);
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
                // The catch parameter is a local like any other: its source spelling and its
                // inline provenance go through the same debug-name boundary, so an expansion's
                // copy is named at the depth it sits at rather than under the bare spelling the
                // source wrote once.
                let rendered = c.binding.as_ref().and_then(|binding| {
                    crate::jvm::debug_local_names::render(
                        self.ir,
                        Some(binding.name.as_str()),
                        binding.provenance,
                    )
                });
                if let (Some(name), Some(start_pc)) = (rendered.as_deref(), local_start) {
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
                    // Same as the normal path: this catch stored the result, and `after` loads it.
                    let parked = result_slot.map(|slot| self.lease_temporary(slot, rt));
                    self.emit(f, code);
                    if let Some(parked) = parked {
                        self.release_temporary(parked);
                    }
                } // `finally` inlined after the catch
                if !fin_diverges {
                    if let Some(f) = finally {
                        debug_lines::mark_block_exit(self.ir, f, code);
                    }
                    // The LAST catch of a `try` with no `finally` is followed immediately by
                    // `after`: nothing stands between them, so the jump would be to the next
                    // instruction. kotlinc falls through there, and the three bytes shift every
                    // offset after them — the exception table's and the line table's alike.
                    // Every other catch has the next handler, or this catch's own copy of the
                    // finalizer, in the way and still needs it.
                    // Falling in leaves the stream live, so `after` is reached without a branch
                    // and its frame is still recorded for the edges that do branch to it.
                    if finally.is_some() || ordinal + 1 != catches.len() {
                        code.goto(after);
                    }
                    after_reachable = true;
                }
            }
            // Preserve catch declaration priority: register every body segment for this catch
            // before moving to the next typed catch.
            for &(range_start, range_end) in &typed_catch_ranges {
                code.add_exception(range_start, range_end, handler, exc_ci);
            }
        }
        self.pending_return_spills.pop();

        // `finally` catch-all: any exception not handled above (in the body or a catch body) runs the
        // `finally` then re-throws. It protects only the body + catch bodies (`fin_ranges`), NOT the
        // inlined finally code — which lies past those ranges, so it doesn't re-catch itself.
        if let Some(f) = finally {
            let mut fin_ranges = self.take_finally_region(f, code);
            let fin_handler = code.new_label();
            // Exception edge — see the `catch` handler above; this one guards the body and every
            // catch body (`fin_ranges`), which are complete by now.
            code.bind_handler(fin_handler, &fin_ranges);
            let thr_ty = Ty::obj("java/lang/Throwable");
            let tslot = parked_slot.expect("a finalizer reserves its parked-exception slot");
            // Live again from here: it holds the caught exception across the whole inlined
            // finalizer, so a `try` inside that copy must not be handed the same slot.
            if let Some(position) = self.free_exception_slots.iter().rposition(|&s| s == tslot) {
                self.free_exception_slots.remove(position);
            }
            // The handler's entry belongs to the finalizer copy it introduces, not to the `finally`
            // keyword — mark it before the store so both copies open on the same line.
            debug_lines::mark_block_entry(self.ir, f, code);
            store(thr_ty, tslot, code);
            // kotlinc protects the handler's own entry — everything ahead of the copy of the
            // finalizer it introduces — with a range of its own.
            let handler_protected_end = code.new_label();
            self.bind(handler_protected_end, code);
            fin_ranges.push((fin_handler, handler_protected_end));
            // The caught exception is LIVE in `tslot` across the whole inlined `finally` (it is
            // re-raised after it), so every StackMapTable frame recorded WHILE emitting the
            // finalizer — a `finally` containing a `try`/`catch` of its own, say — has to type that
            // slot; otherwise the trailing `aload tslot; athrow` reads what the verifier sees as
            // `top`. It is a backend temporary, not a value, and holds its own lease: nested
            // catch-all handlers each lease their own, and a lease cannot collide with a value id.
            let parked = self.lease_temporary(tslot, thr_ty);
            self.emit(f, code);
            self.release_temporary(parked);
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
            // The result is a backend-owned physical temporary, not a semantic value id. Keep it
            // live from the merge frame at `after` until the load without smuggling a reserved
            // numeric key into the semantic slot map.
            let result_lease = result_slot.map(|slot| self.lease_temporary(slot, rt));
            self.bind(after, code);
            if let Some(slot) = result_slot {
                load(rt, slot, code);
            }
            if let Some(lease) = result_lease {
                self.release_temporary(lease);
            }
        } else {
            // Every path diverges — `after` is dead; bind it so any stray reference resolves, but emit
            // no frame (nothing reaches it) and leave no value (the `try` is `Nothing`-typed).
            self.bind(after, code);
        }
        self.pending_return_spills.pop();
    }

    /// Whether a `return` anywhere inside this `try` parks its value while a finalizer runs.
    ///
    /// A lambda's body is a function of its own and its `return`s belong to it, so the walk stops
    /// there; everything else this `try` owns is reached, including its catch and finally bodies —
    /// a `return` written in either of those parks its value the same way.
    fn parks_a_returned_value(&self, expression: crate::ir::ExprId) -> bool {
        let mut pending = vec![expression];
        let mut seen = std::collections::HashSet::new();
        while let Some(node) = pending.pop() {
            if !seen.insert(node) {
                continue;
            }
            match self.ir.expr(node) {
                crate::ir::IrExpr::Return(Some(_)) => return true,
                crate::ir::IrExpr::Lambda { captures, .. } => {
                    pending.extend(captures.iter().copied())
                }
                _ => crate::ir::for_each_child(&self.ir.exprs, node, &mut |child| {
                    pending.push(child)
                }),
            }
        }
        false
    }

    /// The loop a `break`/`continue` leaves: its continue target, its exit target, and the
    /// finalizer depth at its entry.
    ///
    /// A LABELED transfer names the loop the checker bound it to, and only that loop answers. There
    /// is deliberately no fallback to the innermost loop: a label the emitter's loop stack does not
    /// carry means the transfer and its loop disagree about which loop this is, and jumping to the
    /// innermost one would emit a program that branches somewhere the source never wrote. `None`
    /// instead, so the caller fails the file closed.
    pub(super) fn loop_transfer_target(
        &self,
        label: &Option<String>,
    ) -> Option<(Label, Label, usize)> {
        let entry = match label {
            Some(l) => self
                .loop_stack
                .iter()
                .rev()
                .find(|(_, _, sl, _)| sl.as_deref() == Some(l.as_str())),
            None => self.loop_stack.last(),
        };
        entry.map(|(cont, end, _, depth)| (*cont, *end, *depth))
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
        let Some((cont, end, depth)) = self.loop_transfer_target(label) else {
            self.run.set_emit_error(format!(
                "{} names a loop that is not open here: {}",
                if brk { "break" } else { "continue" },
                label.as_deref().unwrap_or("<unlabeled>"),
            ));
            return;
        };
        let target = if brk { end } else { cont };
        if self.return_finalizers.len() > depth {
            // kotlinc closes the protected region on a transfer that leaves a `try` with a `nop`,
            // so the region ends at an instruction of its own rather than at the first byte of the
            // finalizer copy that follows — the same boundary rule as the `nop` that opens it.
            code.nop();
        }
        let survives = self.emit_transfer_finalizers(depth, code);
        if survives {
            code.goto(target);
        }
        self.reopen_finally_segments(code);
    }
}

/// The loop-transfer contract's own refusal.
///
/// A `break`/`continue` whose label names no open loop needs malformed IR — the checker binds every
/// labeled transfer to a loop that encloses it — so this builds the IR directly. Before, the lookup
/// fell back to the innermost loop, which turned a disagreement between a transfer and its loop into
/// a jump the source never wrote.
#[cfg(test)]
mod tests {
    use crate::ir::{IrConst, IrExpr, IrFile, IrFunction};
    use crate::jvm::ir_emit::invariant_tests::emit_for_test;
    use crate::jvm::ir_emit::EmitRun;
    use crate::types::Ty;

    #[test]
    fn a_break_naming_a_loop_that_is_not_open_is_refused_not_redirected() {
        let mut ir = IrFile::default();
        let condition = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let escape = ir.add_expr(IrExpr::Break {
            label: Some("elsewhere".into()),
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![escape],
            value: None,
        });
        let loop_expression = ir.add_expr(IrExpr::While {
            cond: condition,
            body,
            update: None,
            post_test: false,
            label: Some("here".into()),
        });
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(loop_expression),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });

        let run = EmitRun::default();
        assert!(
            emit_for_test(&ir, "Facade", &run).is_none(),
            "a refused transfer must not produce a class file"
        );
        assert_eq!(
            run.emit_error().as_deref(),
            Some("break names a loop that is not open here: elsewhere"),
        );
    }
}
