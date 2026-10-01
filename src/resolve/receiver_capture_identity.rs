//! Stable identity of one implicit-receiver rung captured by a local or anonymous classifier.
//!
//! The checker's scope identity is an address that does not survive the check. Two classifiers that
//! capture the same rung — a local superclass and the enclosing local class of an inner subclass —
//! must still share one identity, and two rungs that happen to carry the same callable or lambda
//! label must not. This table assigns that identity once per scope rung for the current check.

use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ReceiverCaptureIds {
    next: u32,
    assigned: HashMap<(usize, usize), u32>,
}

impl ReceiverCaptureIds {
    pub(super) fn id(&mut self, scope_identity: (usize, usize)) -> u32 {
        if let Some(id) = self.assigned.get(&scope_identity).copied() {
            return id;
        }
        let id = self.next;
        self.next = id
            .checked_add(1)
            .expect("too many implicit receivers in one bounded checker");
        self.assigned.insert(scope_identity, id);
        id
    }
}

impl super::Checker<'_> {
    /// Closure identity of one implicit-receiver rung. An enclosing class instance is the
    /// classifier's own field, not a closure, so it does not take an id from this table.
    pub(super) fn implicit_receiver_capture_id(
        &mut self,
        class_receiver: bool,
        scope_identity: (usize, usize),
    ) -> Option<u32> {
        (!class_receiver).then(|| self.receiver_capture_ids.id(scope_identity))
    }
}
