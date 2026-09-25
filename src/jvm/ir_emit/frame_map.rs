//! The local-slot frame of the method being emitted: the one owner of every local-slot allocation
//! and release.
//!
//! This mirrors kotlinc's `FrameMapBase` (`codegen/FrameMap.kt`), which `IrFrameMap` keys by symbol:
//! a STACK of entered locals over a `currentSize` cursor. `enter` hands out the cursor and moves it
//! up by the local's width; `leave` pops a keyed local and throws unless that local is the top
//! entry; `enterTemp`/`leaveTemp` do the same for an unnamed temporary; `mark`/`dropTo` release
//! everything entered since a point. The slot numbers kotlinc writes are that stack order,
//! renumbered only by its final gap-closing pass (`local_slots::compact` here).
//!
//! # Keyed locals are reused; temporaries are not yet
//!
//! Leaving a keyed local that is the top live entry moves the cursor back to its slot, as kotlinc's
//! `leave` does, so the statements after a block reuse the slots of the locals the block declared.
//! Unkeyed temporaries are still released without moving the cursor: their placement and release
//! points do not match kotlinc's yet, and a keyed leave below them reclaims their slots only once
//! none of them is live. A release that kotlinc's stack would reject — a local left while something
//! entered after it is still live — is reported under the `slots` trace category instead of
//! failing, and keeps the cursor.
//!
//! Four cursor movements kotlinc's frame does not make are kept as named operations until the
//! stage that removes them: [`FrameMap::rewind_to`] (each copy of a spliced lambda body is laid out
//! from the same base), [`FrameMap::give_back`] (a vararg array returns its slot when nothing was
//! entered above it), [`FrameMap::reserve_through`] (a spliced inline body's locals are claimed
//! without being entered) and [`FrameMap::keep_for_method`] (a temporary whose slot is handed out
//! again after its owner ends).

use super::slot_words;
use crate::types::Ty;

/// Identity of a keyed local: kotlinc's `IrSymbol` key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FrameKey {
    /// Slot 0 of an instance method or constructor, or an object's `<clinit>` instance local.
    Receiver,
    /// A physical parameter no semantic value names — an enum constructor's name and ordinal, a
    /// default stub's mask words and marker — by its position in the physical parameter list.
    Parameter(u16),
    /// A semantic value: a declared parameter, a source local or a catch parameter.
    Value(u32),
    /// A value lowering holds one call operand in (`IrFile::call_operand_bindings`). kotlinc has
    /// no local for it: the operand is on the stack until its call, or in the call's own
    /// argument temporary.
    CallOperand(u32),
}

/// What an unkeyed temporary holds. It only labels trace reports: a temporary's identity is its
/// [`TempSlot`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TempRole {
    /// An operand evaluated ahead of its consumer and held across the operands after it.
    OperandSpill,
    /// The left operand of an eager `&&`/`||` held across a branchy right operand.
    BooleanOperand,
    /// A vararg array under construction.
    VarargArray,
    /// A `try` expression's result, parked across the finalizer.
    TryResult,
    /// A returned value, parked across the finalizers the `return` runs.
    ReturnValue,
    /// The throwable a `finally` catch-all holds across its finalizer copy.
    CaughtException,
    /// An inline function's argument, stored into its parameter slot before the body.
    InlineArgument,
    /// A lambda capture materialized for a spliced lambda body.
    LambdaCapture,
    /// A coroutine machine's `$result`, continuation or suspension-marker local.
    CoroutineMachine,
    /// The `$i$f$<name>` inline-depth marker of an emitted inline function.
    InlineDepthMarker,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Occupant {
    Key(FrameKey),
    Temp(TempRole),
}

#[derive(Debug)]
struct Entry {
    /// Entry order, never reused: a [`Mark`] and a [`TempSlot`] name entries by it, so removing an
    /// entry out of order cannot shift what they refer to.
    id: u32,
    occupant: Occupant,
    slot: u16,
    words: u16,
    /// This entry backs a method-wide reuse pool, so an inline-call/frame rewind must not discard
    /// its reservation even when the entry was created after the rewind mark.
    kept_for_method: bool,
}

/// A live unkeyed temporary. Not `Copy`: leaving it consumes it, so it is left at most once.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct TempSlot {
    id: u32,
    slot: u16,
    words: u16,
}

impl TempSlot {
    pub(super) fn slot(&self) -> u16 {
        self.slot
    }
}

/// A point in the frame's history: kotlinc's `FrameMapBase.Mark`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Mark {
    id: u32,
    size: u16,
}

#[derive(Default, Debug)]
pub(super) struct FrameMap {
    /// The next free slot: kotlinc's `currentSize`.
    size: u16,
    /// The highest `size` has reached.
    max: u16,
    /// The live entries, oldest first.
    entries: Vec<Entry>,
    next_id: u32,
}

impl FrameMap {
    /// The next free slot.
    pub(super) fn size(&self) -> u16 {
        self.size
    }

    /// The width every local handed out so far fits under.
    pub(super) fn max(&self) -> u16 {
        self.max
    }

    /// Enter a keyed local of type `ty` and return its slot.
    pub(super) fn enter(&mut self, key: FrameKey, ty: Ty) -> u16 {
        self.push(Occupant::Key(key), ty).1
    }

    /// Enter an unkeyed temporary of type `ty`.
    pub(super) fn enter_temp(&mut self, role: TempRole, ty: Ty) -> TempSlot {
        let (id, slot) = self.push(Occupant::Temp(role), ty);
        TempSlot {
            id,
            slot,
            words: slot_words(ty),
        }
    }

    /// Leave the most recently entered local keyed `key`.
    pub(super) fn leave(&mut self, key: FrameKey) {
        let occupant = Occupant::Key(key);
        match self
            .entries
            .iter()
            .rposition(|entry| entry.occupant == occupant)
        {
            Some(index) => self.leave_at(index),
            None => crate::trace_compiler!("slots", "leave {key:?}: never entered"),
        }
    }

    /// Leave a temporary.
    pub(super) fn leave_temp(&mut self, temp: TempSlot) {
        match self.entries.iter().rposition(|entry| entry.id == temp.id) {
            Some(index) => self.leave_at(index),
            None => crate::trace_compiler!(
                "slots",
                "leave temporary at slot {}: already dropped",
                temp.slot
            ),
        }
    }

    /// Leave the keyed locals entered since `mark`, newest first, as kotlinc's block end leaves the
    /// variables it declared. Temporaries entered since then stay: each is left by its owner.
    pub(super) fn leave_block(&mut self, mark: Mark) {
        let mut index = self.entries.len();
        while index > 0 {
            index -= 1;
            let entry = &self.entries[index];
            if entry.id < mark.id {
                break;
            }
            if matches!(entry.occupant, Occupant::Key(_)) {
                self.leave_at(index);
            }
        }
    }

    /// The frame size below call-operand holders that already occupy their target parameter slots.
    /// Only a top run belonging to this call is considered, and every holder must have the same
    /// slot and width after the callee's parameters are laid out from the candidate base. Thus the
    /// spliced parameter stores write each holder's own value back into its own slot; reordered,
    /// duplicated, or shifted operands conservatively keep the current size.
    pub(super) fn aligned_call_operand_base(&self, parameters: &[(Option<u32>, u16)]) -> u16 {
        let mut base = self.size;
        let mut holders = Vec::new();
        for entry in self.entries.iter().rev() {
            match entry.occupant {
                Occupant::Key(FrameKey::CallOperand(value))
                    if parameters
                        .iter()
                        .any(|(operand, _)| *operand == Some(value))
                        && entry.slot < base =>
                {
                    base = entry.slot;
                    holders.push((value, entry.slot, entry.words));
                }
                _ => break,
            }
        }
        if holders.is_empty() {
            return self.size;
        }
        let mut target = base;
        let mut matched = 0;
        for &(operand, words) in parameters {
            if let Some(value) = operand {
                if let Some(&(_, slot, held_words)) =
                    holders.iter().find(|&&(held, _, _)| held == value)
                {
                    let unique = parameters
                        .iter()
                        .filter(|(candidate, _)| *candidate == Some(value))
                        .count()
                        == 1;
                    if !unique || slot != target || held_words != words {
                        return self.size;
                    }
                    matched += 1;
                }
            }
            let Some(next) = target.checked_add(words) else {
                return self.size;
            };
            target = next;
        }
        if matched == holders.len() {
            base
        } else {
            self.size
        }
    }

    pub(super) fn mark(&self) -> Mark {
        Mark {
            id: self.next_id,
            size: self.size,
        }
    }

    /// Release every call-local entry made since `mark`, in any order, as kotlinc's `Mark.dropTo`
    /// does. Method-wide reservations survive: their numeric slots remain in a reuse pool after the
    /// construct that first entered them.
    pub(super) fn drop_to(&mut self, mark: Mark) {
        self.entries
            .retain(|entry| entry.id < mark.id || entry.kept_for_method);
    }

    /// Release everything entered since `mark` AND move the cursor back to it: the next spliced copy
    /// of a lambda body is laid out from the same base as the one before it.
    pub(super) fn rewind_to(&mut self, mark: Mark) {
        self.drop_to(mark);
        self.size = self
            .entries
            .iter()
            .filter(|entry| entry.kept_for_method)
            .map(|entry| entry.slot + entry.words)
            .fold(mark.size, u16::max);
    }

    /// Leave a temporary, and return its slot to the cursor when nothing was allocated above it
    /// since — the one reuse a vararg array has always made.
    pub(super) fn give_back(&mut self, temp: TempSlot) {
        let (slot, words) = (temp.slot, temp.words);
        self.leave_temp(temp);
        if self.size == slot + words {
            self.size = slot;
        }
    }

    /// Claim every slot below `top` without entering it: a spliced inline body lays its own locals
    /// out above its base, and the frame learns only how far they reach.
    pub(super) fn reserve_through(&mut self, top: u16) {
        self.size = self.size.max(top);
        self.max = self.max.max(self.size);
    }

    fn push(&mut self, occupant: Occupant, ty: Ty) -> (u32, u16) {
        let id = self.next_id;
        self.next_id += 1;
        let slot = self.size;
        let words = slot_words(ty);
        self.entries.push(Entry {
            id,
            occupant,
            slot,
            words,
            kept_for_method: false,
        });
        self.size += words;
        self.max = self.max.max(self.size);
        (id, slot)
    }

    /// Keep a temporary entered for the rest of the method: its owner hands the slot out again after
    /// the code that entered it ends, so no later local may be given it.
    pub(super) fn keep_for_method(&mut self, temp: TempSlot) {
        match self.entries.iter_mut().find(|entry| entry.id == temp.id) {
            Some(entry) => entry.kept_for_method = true,
            None => crate::trace_compiler!(
                "slots",
                "keep temporary at slot {} for method: already dropped",
                temp.slot
            ),
        }
    }

    /// Remove one entry. kotlinc throws "Descriptor can be left only if it is last" when it is not
    /// the top one; here that is reported and the cursor is kept. A keyed local left from the top
    /// returns the cursor to its slot; a temporary keeps it.
    fn leave_at(&mut self, index: usize) {
        let entry = self.entries.remove(index);
        match self.entries.get(index) {
            None if matches!(entry.occupant, Occupant::Key(_)) => self.size = entry.slot,
            None => {}
            Some(above) => crate::trace_compiler!(
                "slots",
                "leave {:?} at slot {} is not last: {:?} at slot {} was entered after it and is live ({} above)",
                entry.occupant,
                entry.slot,
                above.occupant,
                above.slot,
                self.entries.len() - index
            ),
        }
    }

    #[cfg(test)]
    fn occupants(&self) -> Vec<(Occupant, u16)> {
        self.entries
            .iter()
            .map(|entry| (entry.occupant, entry.slot))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_hands_out_the_cursor_by_width() {
        let mut frame = FrameMap::default();
        assert_eq!(frame.enter(FrameKey::Receiver, Ty::obj("A")), 0);
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Long), 1);
        assert_eq!(frame.enter(FrameKey::Value(2), Ty::Int), 3);
        let temp = frame.enter_temp(TempRole::OperandSpill, Ty::Double);
        assert_eq!(temp.slot(), 4);
        assert_eq!((frame.size(), frame.max()), (6, 6));
    }

    #[test]
    fn a_unit_local_takes_no_slot() {
        let mut frame = FrameMap::default();
        assert_eq!(frame.enter(FrameKey::Value(0), Ty::Unit), 0);
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Int), 0);
        assert_eq!(frame.size(), 1);
    }

    #[test]
    fn a_block_end_returns_its_locals_slots_to_the_next_statement() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Value(0), Ty::Boolean);
        let then_branch = frame.mark();
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Int), 1);
        frame.leave_block(then_branch);
        let else_branch = frame.mark();
        assert_eq!(frame.enter(FrameKey::Value(2), Ty::Long), 1);
        frame.leave_block(else_branch);
        assert_eq!(frame.enter(FrameKey::Value(3), Ty::Int), 1);
        assert_eq!((frame.size(), frame.max()), (2, 3));
    }

    #[test]
    fn a_call_reuses_only_operand_holders_aligned_with_its_parameter_slots() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Value(0), Ty::Int);
        frame.enter(FrameKey::CallOperand(1), Ty::Long);
        assert_eq!(frame.enter(FrameKey::CallOperand(2), Ty::obj("A")), 3);
        assert_eq!(
            frame.aligned_call_operand_base(&[(Some(1), 2), (Some(2), 1)]),
            1
        );
        assert_eq!(
            frame.aligned_call_operand_base(&[(Some(2), 1), (Some(1), 2)]),
            4
        );
        assert_eq!(
            frame.aligned_call_operand_base(&[(Some(1), 2), (Some(1), 2)]),
            4
        );
        assert_eq!(frame.aligned_call_operand_base(&[(Some(2), 1)]), 3);
        assert_eq!(
            frame.aligned_call_operand_base(&[(None, 1), (Some(2), 1)]),
            4
        );
        frame.enter(FrameKey::Value(3), Ty::Int);
        assert_eq!(
            frame.aligned_call_operand_base(&[(Some(1), 2), (Some(2), 1)]),
            5
        );
    }

    #[test]
    fn a_temporary_release_keeps_the_cursor() {
        let mut frame = FrameMap::default();
        let temp = frame.enter_temp(TempRole::BooleanOperand, Ty::Boolean);
        frame.leave_temp(temp);
        assert_eq!(frame.size(), 1);
        assert_eq!(frame.enter(FrameKey::Value(0), Ty::Int), 1);
    }

    #[test]
    fn a_keyed_leave_reclaims_released_temporaries_above_it() {
        let mut frame = FrameMap::default();
        let block = frame.mark();
        frame.enter(FrameKey::Value(0), Ty::Int);
        let temp = frame.enter_temp(TempRole::BooleanOperand, Ty::Boolean);
        frame.leave_temp(temp);
        frame.leave_block(block);
        assert!(frame.occupants().is_empty());
        assert_eq!((frame.size(), frame.max()), (0, 2));
    }

    #[test]
    fn a_keyed_leave_under_a_live_entry_keeps_the_cursor() {
        let mut frame = FrameMap::default();
        let block = frame.mark();
        frame.enter(FrameKey::Value(0), Ty::Int);
        let _kept = frame.enter_temp(TempRole::CaughtException, Ty::obj("T"));
        frame.leave_block(block);
        assert_eq!(frame.size(), 2);
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Int), 2);
    }

    #[test]
    fn a_temporary_kept_for_the_method_is_never_reclaimed() {
        let mut frame = FrameMap::default();
        let block = frame.mark();
        frame.enter(FrameKey::Value(0), Ty::Int);
        let parked = frame.enter_temp(TempRole::CaughtException, Ty::obj("T"));
        frame.keep_for_method(parked);
        frame.leave_block(block);
        assert_eq!(
            frame.occupants(),
            vec![(Occupant::Temp(TempRole::CaughtException), 1)]
        );
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Int), 2);
    }

    #[test]
    fn a_method_kept_temporary_survives_a_rewind_to_an_earlier_mark() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Receiver, Ty::obj("A"));
        let inline_call = frame.mark();
        let parked = frame.enter_temp(TempRole::CaughtException, Ty::obj("java/lang/Throwable"));
        frame.keep_for_method(parked);
        frame.enter(FrameKey::Value(0), Ty::Long);

        frame.rewind_to(inline_call);

        assert_eq!(
            frame.occupants(),
            vec![
                (Occupant::Key(FrameKey::Receiver), 0),
                (Occupant::Temp(TempRole::CaughtException), 1),
            ]
        );
        assert_eq!(frame.size(), 2);
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Int), 2);
    }

    #[test]
    fn leave_pops_the_newest_entry_with_that_key() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Value(7), Ty::Int);
        frame.enter(FrameKey::Value(7), Ty::Int);
        frame.leave(FrameKey::Value(7));
        assert_eq!(
            frame.occupants(),
            vec![(Occupant::Key(FrameKey::Value(7)), 0)]
        );
    }

    #[test]
    fn a_leave_out_of_stack_order_still_removes_only_that_entry() {
        let mut frame = FrameMap::default();
        let first = frame.enter_temp(TempRole::OperandSpill, Ty::Int);
        let second = frame.enter_temp(TempRole::OperandSpill, Ty::Int);
        frame.leave_temp(first);
        assert_eq!(
            frame.occupants(),
            vec![(Occupant::Temp(TempRole::OperandSpill), 1)]
        );
        frame.leave_temp(second);
        assert!(frame.occupants().is_empty());
        assert_eq!(frame.size(), 2);
    }

    #[test]
    fn a_block_leaves_its_keyed_locals_and_keeps_live_temporaries() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Value(0), Ty::Int);
        let block = frame.mark();
        frame.enter(FrameKey::Value(1), Ty::Int);
        let temp = frame.enter_temp(TempRole::TryResult, Ty::Int);
        frame.enter(FrameKey::Value(2), Ty::Long);
        frame.leave_block(block);
        assert_eq!(
            frame.occupants(),
            vec![
                (Occupant::Key(FrameKey::Value(0)), 0),
                (Occupant::Temp(TempRole::TryResult), 2),
            ]
        );
        frame.leave_temp(temp);
        assert_eq!(frame.occupants().len(), 1);
    }

    #[test]
    fn a_mark_survives_an_earlier_entry_leaving_out_of_order() {
        let mut frame = FrameMap::default();
        let spill = frame.enter_temp(TempRole::OperandSpill, Ty::Int);
        let block = frame.mark();
        frame.enter(FrameKey::Value(3), Ty::Int);
        frame.leave_temp(spill);
        frame.leave_block(block);
        assert!(frame.occupants().is_empty());
    }

    #[test]
    fn drop_to_releases_everything_since_the_mark_without_moving_the_cursor() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Receiver, Ty::obj("A"));
        let call = frame.mark();
        let _capture = frame.enter_temp(TempRole::LambdaCapture, Ty::obj("B"));
        frame.enter(FrameKey::Value(4), Ty::Int);
        frame.drop_to(call);
        assert_eq!(
            frame.occupants(),
            vec![(Occupant::Key(FrameKey::Receiver), 0)]
        );
        assert_eq!(frame.size(), 3);
    }

    #[test]
    fn rewind_lays_the_next_copy_out_from_the_same_base() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Value(0), Ty::Int);
        let copies = frame.mark();
        let first = frame.enter(FrameKey::Value(1), Ty::Long);
        frame.rewind_to(copies);
        assert!(frame.occupants().len() == 1 && frame.size() == 1);
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Long), first);
        assert_eq!(frame.max(), 3);
    }

    #[test]
    fn give_back_returns_a_top_temporary_only() {
        let mut frame = FrameMap::default();
        let array = frame.enter_temp(TempRole::VarargArray, Ty::obj("A"));
        frame.give_back(array);
        assert_eq!(frame.size(), 0);

        let array = frame.enter_temp(TempRole::VarargArray, Ty::obj("A"));
        let _above = frame.enter_temp(TempRole::OperandSpill, Ty::Int);
        frame.give_back(array);
        assert_eq!(frame.size(), 2);
        assert_eq!(frame.max(), 2);
    }

    #[test]
    fn reserve_through_only_raises() {
        let mut frame = FrameMap::default();
        frame.enter(FrameKey::Value(0), Ty::Int);
        frame.reserve_through(5);
        frame.reserve_through(3);
        assert_eq!((frame.size(), frame.max()), (5, 5));
        assert_eq!(frame.enter(FrameKey::Value(1), Ty::Int), 5);
    }
}
