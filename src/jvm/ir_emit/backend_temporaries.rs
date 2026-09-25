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

use super::VerifType;
use crate::types::Ty;

/// Identity of a leased backend temporary slot. A newtype on purpose: it is not a value id and
/// cannot be mistaken for one, which is what the reserved numeric ranges could not guarantee.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct TemporaryLease(u32);

/// The temporaries live right now, oldest first.
#[derive(Default)]
pub(super) struct BackendTemporaries {
    /// Insertion-ordered, so a later lease over the same slot wins — and so a frame's layout does
    /// not depend on hash iteration order.
    held: Vec<(TemporaryLease, u16, Ty)>,
    /// Monotonic for the whole emitter: a released lease's identity is never handed out again, so a
    /// stale release cannot free a live temporary.
    next: u32,
}

impl BackendTemporaries {
    /// Take a slot for as long as it is live. The returned lease is the only way to give it back.
    pub(super) fn lease(&mut self, slot: u16, ty: Ty) -> TemporaryLease {
        let lease = TemporaryLease(self.next);
        self.next += 1;
        self.held.push((lease, slot, ty));
        lease
    }

    /// Give a leased temporary back. Its slot stops appearing in frames recorded from here on;
    /// the emitter's slot allocator stays monotonic, so the slot itself is not reused behind the
    /// verifier's back.
    pub(super) fn release(&mut self, lease: TemporaryLease) {
        let before = self.held.len();
        self.held.retain(|(current, _, _)| *current != lease);
        debug_assert_eq!(
            before - self.held.len(),
            1,
            "released a backend temporary that was not leased"
        );
    }

    /// The live `(slot, type)` pairs, in lease order.
    pub(super) fn live(&self) -> Vec<(u16, Ty)> {
        self.held.iter().map(|(_, slot, ty)| (*slot, *ty)).collect()
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
