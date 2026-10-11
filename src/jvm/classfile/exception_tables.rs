//! Recording and final ordering of a method's `Code` exception table.

use super::{CodeBuilder, Label};

impl CodeBuilder {
    /// The method expanded inline bytecode, so [`Self::resolved_exceptions`] sorts the table the
    /// way kotlinc's inliner does.
    pub(crate) fn note_inlined_bytecode(&mut self) {
        self.inlined_bytecode = true;
    }

    /// Register a `try` range `[start, end)` guarded by a handler at `handler`, catching
    /// `catch_type` (a constant-pool class index, or 0 for catch-all).
    pub fn add_exception(&mut self, start: Label, end: Label, handler: Label, catch_type: u16) {
        self.exceptions.push((start, end, handler, catch_type));
    }

    /// Resolve the exception table to byte offsets after all labels are bound. Degenerate ranges
    /// protect nothing and are illegal in a class file, so they are omitted.
    pub fn resolved_exceptions(&self) -> Vec<(u16, u16, u16, u16)> {
        let mut resolved = self
            .exceptions
            .iter()
            // An unbound label belongs to a dead region dropped by `bind_at`; fabricating offset
            // 65535 from its sentinel would create a range over bytes that do not exist.
            .filter(|&&(start, end, handler, _)| {
                [start, end, handler]
                    .iter()
                    .all(|&label| self.labels[self.label_index(label)] != usize::MAX)
            })
            .map(|&(start, end, handler, catch_type)| {
                (
                    self.labels[self.label_index(start)] as u16,
                    self.labels[self.label_index(end)] as u16,
                    self.labels[self.label_index(handler)] as u16,
                    catch_type,
                )
            })
            .filter(|&(start, end, _, _)| start < end)
            .collect::<Vec<_>>();
        // `(start, end, handler, type)`. kotlinc's inliner sorts by handler index, then start.
        if self.inlined_bytecode {
            resolved.sort_by(|left, right| left.2.cmp(&right.2).then(left.0.cmp(&right.0)));
        }
        resolved
    }
}
