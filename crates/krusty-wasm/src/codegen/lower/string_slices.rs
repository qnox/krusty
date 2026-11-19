//! `String.subSequence(startIndex, endIndex)` on a string the runtime lays out: the call site
//! checks the range as Kotlin does, then the runtime copies the units.

use super::*;

const I32_ADD: u8 = 0x6a;

impl Body<'_> {
    /// Evaluate the receiver and both indices, in that order, throw `IndexOutOfBoundsException`
    /// unless `0 <= start <= end <= length`, and leave the copied string on the stack.
    pub(super) fn string_sub_sequence(
        &mut self,
        receiver: u32,
        start: u32,
        end: u32,
    ) -> Result<(), Unsupported> {
        let string = self.local(self.runtime.nullable_string_type());
        let (from, to) = (self.local(ValType::I32), self.local(ValType::I32));
        self.value(receiver, Ty::String)?;
        self.code.local_set(string);
        self.value(start, Ty::Int)?;
        self.code.local_set(from);
        self.value(end, Ty::Int)?;
        self.code.local_set(to);
        // `end <= length`, then `start <= end`; an unsigned comparison also rejects a negative
        // index, and the first check keeps `end + 1` from overflowing the second.
        self.code
            .local_get(to)
            .local_get(string)
            .struct_get(self.runtime.string, 0)
            .array_len()
            .i32_const(1)
            .op(I32_ADD);
        self.throw_unless_index_below()?;
        self.code
            .local_get(from)
            .local_get(to)
            .i32_const(1)
            .op(I32_ADD);
        self.throw_unless_index_below()?;
        let slice = self.runtime.string_sub_sequence(self.module);
        self.code
            .local_get(string)
            .local_get(from)
            .local_get(to)
            .call(slice);
        Ok(())
    }
}
