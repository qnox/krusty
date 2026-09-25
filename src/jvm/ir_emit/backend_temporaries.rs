//! The slots the BACKEND owns, and the one place a frame's locals are laid out.
//!
//! A backend temporary is a local no semantic value names: an operand spilled across a branch, a
//! `&&` operand held across its right-hand side, a vararg array under construction, a `try` result
//! parked across a `finally`, a catch-all's caught throwable, a return parked across a finalizer, a
//! default stub's mask words and marker. Each has to be typed in every frame recorded while it is
//! live, and in none recorded after it dies.
//!
//! They used to be registered in the emitter's SEMANTIC slot map under reserved numeric ranges —
//! `1_000_000` for a boolean spill, `2_000_000` for an operand or vararg array, `3_000_000` for a
//! `try` result, `4_000_000` for a caught throwable, `5_000_000` for a parked return, `9_000_001`
//! for a stub's masks. Nothing but the size of a real value id kept those apart, and `3_000_000`
//! was a constant, so a `try` inside another `try`'s `finally` reused it outright. Living in that
//! map also exposed them to machinery meant for semantic locals: definite-assignment filtering,
//! lexical-scope removal, and the save/restore an inline body performs around its own frame.
//!
//! [`TemporaryLease`] is a distinct type, so a temporary's identity cannot be a value id. And
//! because a frame is only correct when BOTH populations are laid out together, [`frame_slots`] is
//! the single operation that does it — the previous arrangement had three frame builders, two of
//! which silently omitted the temporaries and were right only by accident.

use super::frame_map::{TempRole, TempSlot};
use super::{store, Emitter, VerifType};
use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

/// Identity of a leased backend temporary slot. A newtype on purpose: it is not a value id and
/// cannot be mistaken for one, which is what the reserved numeric ranges could not guarantee.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct TemporaryLease(u32);

struct Held {
    lease: TemporaryLease,
    slot: u16,
    ty: Ty,
    /// The frame entry the slot was taken from, when the lease owns it: releasing the lease leaves
    /// it.
    entered: Option<TempSlot>,
}

/// The temporaries live right now, oldest first.
#[derive(Default)]
pub(super) struct BackendTemporaries {
    /// Insertion-ordered, so a later lease over the same slot wins — and so a frame's layout does
    /// not depend on hash iteration order.
    held: Vec<Held>,
    /// Monotonic for the whole emitter: a released lease's identity is never handed out again, so a
    /// stale release cannot free a live temporary.
    next: u32,
}

impl BackendTemporaries {
    /// Take a slot for as long as it is live. The returned lease is the only way to give it back.
    fn lease(&mut self, slot: u16, ty: Ty, entered: Option<TempSlot>) -> TemporaryLease {
        let lease = TemporaryLease(self.next);
        self.next += 1;
        self.held.push(Held {
            lease,
            slot,
            ty,
            entered,
        });
        lease
    }

    /// Give a leased temporary back. Its slot stops appearing in frames recorded from here on.
    /// Returns the frame entry the lease owned, for the caller to leave.
    fn release(&mut self, lease: TemporaryLease) -> Option<TempSlot> {
        let index = self.held.iter().position(|held| held.lease == lease);
        debug_assert!(
            index.is_some(),
            "released a backend temporary that was not leased"
        );
        index.and_then(|index| self.held.remove(index).entered)
    }

    /// The live `(slot, type)` pairs, in lease order.
    pub(super) fn live(&self) -> Vec<(u16, Ty)> {
        self.held.iter().map(|held| (held.slot, held.ty)).collect()
    }
}

impl Emitter<'_> {
    /// Type a slot the backend owns in every frame while it is live. The slot's frame entry, if it
    /// has one, stays with whoever entered it.
    pub(super) fn lease_temporary(&mut self, slot: u16, ty: Ty) -> TemporaryLease {
        self.lease(slot, ty, None)
    }

    /// Type a temporary just entered in the frame while it is live; releasing the lease leaves it.
    pub(super) fn lease_frame_temporary(&mut self, temp: TempSlot, ty: Ty) -> TemporaryLease {
        self.lease(temp.slot(), ty, Some(temp))
    }

    pub(super) fn release_temporary(&mut self, lease: TemporaryLease) {
        if let Some(temp) = self.temporaries.release(lease) {
            self.frame.leave_temp(temp);
        }
    }

    /// Evaluate each of `ops` into a fresh temporary, in order. Each is leased, so a later operand's
    /// frames see the earlier ones as live rather than `Top`; the caller loads them and then hands
    /// them to [`Self::release_operand_spills`]. Returns `(slot, ty, lease)` per operand.
    pub(super) fn spill_to_temps(
        &mut self,
        ops: &[u32],
        code: &mut CodeBuilder,
    ) -> Vec<(u16, Ty, TemporaryLease)> {
        let mut temps = Vec::new();
        for &o in ops {
            self.emit_value(o, code);
            let t = self.value_ty(o);
            crate::trace_compiler!(
                "splice",
                "spill inline operand expression={o} node={:?} type={t:?}",
                self.ir.expr(o)
            );
            let temp = self.frame.enter_temp(TempRole::OperandSpill, t);
            let slot = temp.slot();
            store(t, slot, code);
            let lease = self.lease_frame_temporary(temp, t);
            temps.push((slot, t, lease));
        }
        temps
    }

    /// Release spilled operands once they are loaded, newest first as they were entered, so the
    /// frame's cursor returns below the first of them.
    pub(super) fn release_operand_spills(&mut self, temps: &[(u16, Ty, TemporaryLease)]) {
        for &(_, _, lease) in temps.iter().rev() {
            self.release_temporary(lease);
        }
    }

    fn lease(&mut self, slot: u16, ty: Ty, entered: Option<TempSlot>) -> TemporaryLease {
        debug_assert!(
            !self.slots.values().any(|(held, _)| *held == slot),
            "backend temporary at slot {slot} aliases a semantic local"
        );
        // A temporary released before the plan is read is invisible to it otherwise, and a spill
        // set cannot skip a slot the frames still describe.
        self.temporaries.lease(slot, ty, entered)
    }
}

/// Lay out one frame's locals for slots `0..upto`, slot-indexed, `Top` in the gaps.
///
/// The SEMANTIC locals go down first and the temporaries over them: a temporary's slot is allocated
/// above every semantic one, so only a stale semantic entry can sit under it. Both populations are
/// applied here and nowhere else — a frame that carries one and not the other is what the verifier
/// rejects, in either direction.
pub(super) fn frame_slots(
    upto: u16,
    semantic: &[(u16, Ty)],
    temporaries: &[(u16, Ty)],
    verif: &mut impl FnMut(Ty) -> VerifType,
) -> Vec<VerifType> {
    let mut raw = vec![VerifType::Top; upto as usize];
    for (slot, ty) in semantic.iter().chain(temporaries).copied() {
        if (slot as usize) < raw.len() {
            raw[slot as usize] = verif(ty);
        }
    }
    raw
}
