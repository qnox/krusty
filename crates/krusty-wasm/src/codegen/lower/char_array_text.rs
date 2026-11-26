//! `CharArray.concatToString` and the `String(CharArray)` factories on a char array this target
//! lays out itself: the runtime function checks the range as the stdlib does and copies the units.

use krusty::libraries::CharArrayRange;

use super::*;

impl Body<'_> {
    /// Evaluate the char array and the range's indices, in that order, and leave the string on the
    /// stack. `args` is the array first, as an extension receiver or the first argument is.
    pub(super) fn char_array_to_string(
        &mut self,
        range: CharArrayRange,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let (array, indices) = match (range, args) {
            (CharArrayRange::Whole, [array]) => (*array, None),
            (CharArrayRange::StartEnd | CharArrayRange::OffsetLength, [array, first, second]) => {
                (*array, Some((*first, *second)))
            }
            _ => {
                return Err("an authenticated char array conversion with malformed operands".into())
            }
        };
        let chars = Ty::array(Ty::Char);
        let (form, _) = self.array_form(chars)?;
        self.value(array, chars)?;
        self.array_storage(form);
        match indices {
            Some((first, second)) => {
                self.value(first, Ty::Int)?;
                self.value(second, Ty::Int)?;
            }
            None => {
                self.code.i32_const(0).i32_const(0);
            }
        }
        let function = self
            .runtime
            .char_array_to_string(self.module, self.classifiers, range)?;
        self.code.call(function);
        Ok(())
    }
}
