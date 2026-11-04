//! The debug locals a byte splice places into the caller's table.

use super::*;

impl Emitter<'_> {
    /// Describe a spliced body's own locals in the caller's debug table.
    ///
    /// They are real locals of the method that now contains them; the reference compiler names every
    /// one. `shift` is where the splice landed for a body laid out at offset 0 — a branchless splice
    /// is appended wherever the caller happens to be — and zero for one already laid out in place.
    pub(super) fn record_spliced_locals(
        &mut self,
        locals: &[(u16, u16, u16, String, String)],
        inline_only: bool,
        shift: usize,
        code: &mut CodeBuilder,
    ) {
        // An `@InlineOnly` body contributes no debug locals either, for the same reason.
        if !self.record_locals || inline_only {
            return;
        }
        for (start, length, slot, name, descriptor) in locals {
            let Ok(start) = u16::try_from(*start as usize + shift) else {
                continue;
            };
            code.add_local_entry(start, Some(*length), *slot, name, descriptor);
        }
    }
}
