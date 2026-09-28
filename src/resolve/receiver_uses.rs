//! Semantic uses of exact lexical receiver bindings, counted and kept in the order they happened.
//!
//! Capture discovery snapshots the counts around a nested classifier check so qualified
//! member-extension calls still capture their implicit dispatch receiver, and orders the captured
//! receivers by the body's first use of each, as kotlinc's local declaration lowering does. The
//! scope identity, unlike type or name matching, tells same-typed receiver rungs apart.

use std::collections::HashMap;

use crate::diag::Span;

/// One use, keyed by the binding's scope identity or, for an extension receiver named through its
/// label or its declaration, by that declaration's span.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiverUse {
    Binding((usize, usize)),
    Extension(Span),
}

#[derive(Default)]
pub(super) struct ReceiverUses {
    counts: HashMap<(usize, usize), usize>,
    order: Vec<ReceiverUse>,
}

impl ReceiverUses {
    pub(super) fn record(&mut self, identity: (usize, usize)) {
        *self.counts.entry(identity).or_default() += 1;
        self.order.push(ReceiverUse::Binding(identity));
    }

    pub(super) fn record_extension(&mut self, declaration: Span) {
        self.order.push(ReceiverUse::Extension(declaration));
    }

    pub(super) fn count(&self, identity: (usize, usize)) -> usize {
        self.counts.get(&identity).copied().unwrap_or_default()
    }

    /// A position to measure later uses from.
    pub(super) fn mark(&self) -> usize {
        self.order.len()
    }

    /// Where the receiver bound at `identity`, or declared as the extension receiver at
    /// `extension`, was first used after `mark`: `usize::MAX` when it was not.
    pub(super) fn first_use_since(
        &self,
        mark: usize,
        identity: (usize, usize),
        extension: Option<Span>,
    ) -> usize {
        self.order[mark..]
            .iter()
            .position(|used| match *used {
                ReceiverUse::Binding(used) => used == identity,
                ReceiverUse::Extension(used) => Some(used) == extension,
            })
            .unwrap_or(usize::MAX)
    }
}
