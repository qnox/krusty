//! `String.subSequence(startIndex, endIndex)` on a string the runtime lays out: the runtime
//! function checks the range as Kotlin does and copies the units.

use super::*;

impl Body<'_> {
    /// Evaluate the receiver and both indices, in that order, and leave the checked copy on the
    /// stack.
    pub(super) fn string_sub_sequence(
        &mut self,
        receiver: u32,
        start: u32,
        end: u32,
    ) -> Result<(), Unsupported> {
        self.value(receiver, Ty::String)?;
        self.value(start, Ty::Int)?;
        self.value(end, Ty::Int)?;
        let slice = self
            .runtime
            .string_sub_sequence(self.module, self.classifiers)?;
        self.code.call(slice);
        Ok(())
    }
}
