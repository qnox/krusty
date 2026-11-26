//! `String.subSequence(Int, Int)`: the characters of a string between two indices, itself a string.
//!
//! The function checks the range as Kotlin does, throwing `IndexOutOfBoundsException` unless
//! `0 <= start <= end <= length`, then copies the units into a new array; a module carries it only
//! once a call needs it. A direct call and the `CharSequence` dispatch entry share it, so neither
//! route can skip the check.

use super::*;
use krusty::backend::BackendClassifierSource;
use krusty::libraries::RuntimeExceptionKind;

const GE_U32: u8 = 0x4f;

impl Runtime {
    /// The slice function, once a call or a `CharSequence` implementation has defined it.
    pub(crate) fn defined_string_sub_sequence(&self) -> Option<u32> {
        self.string_sub_sequence
    }

    /// The function `(s: String, start: Int, end: Int) -> String` copying `s[start until end]`,
    /// defined on first use.
    pub(crate) fn string_sub_sequence(
        &mut self,
        module: &mut Module,
        source: &dyn BackendClassifierSource,
    ) -> Result<u32, String> {
        if let Some(function) = self.string_sub_sequence {
            return Ok(function);
        }
        let exception = self.exception_class(
            module,
            source,
            RuntimeExceptionKind::IndexOutOfBounds.classifier(),
        )?;
        // The receiver may be carried nullable; the checked call proved it is not null.
        let receiver = self.nullable_string_type();
        let result = ValType::reference(self.string);
        let ty = module.func_type(vec![receiver, ValType::I32, ValType::I32], vec![result]);
        let (string, start, end, units, copy) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(string)
            .struct_get(self.string, 0)
            .local_set(units);
        // `end <= length`, then `start <= end`; an unsigned comparison also rejects a negative
        // index, and the first check keeps `end + 1` from overflowing the second.
        for check in 0..2 {
            if check == 0 {
                code.local_get(end).local_get(units).array_len();
            } else {
                code.local_get(start).local_get(end);
            }
            code.i32_const(1).op(ADD32).op(GE_U32).if_(BlockType::Empty);
            self.exceptions
                .construct_class(&mut code, exception, None, self.string)?;
            code.throw(self.exceptions.tag).end();
        }
        code.local_get(end)
            .local_get(start)
            .op(SUB32)
            .array_new_default(self.chars)
            .local_set(copy);
        // array.copy dst dst_offset src src_offset length
        code.local_get(copy)
            .i32_const(0)
            .local_get(units)
            .local_get(start)
            .local_get(end)
            .local_get(start)
            .op(SUB32)
            .array_copy(self.chars, self.chars);
        code.local_get(copy).struct_new(self.string);
        let chars = ValType::reference(self.chars);
        let locals = vec![chars, chars];
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        self.string_sub_sequence = Some(function);
        Ok(function)
    }
}
