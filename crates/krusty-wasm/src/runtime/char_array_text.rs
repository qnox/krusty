//! The string of a range of a char array: `CharArray.concatToString` and the `String(CharArray)`
//! factories of `kotlin.text`.
//!
//! The function checks the range as the stdlib does, throwing before it copies anything, then
//! copies the units into a new array that is the string's storage. A module carries one function
//! per kind of range, and only once a call needs it.

use super::*;
use krusty::backend::BackendClassifierSource;
use krusty::libraries::{CharArrayRange, RuntimeExceptionKind};
use krusty::types::Ty;

impl Runtime {
    /// The function `(chars: CharArray storage, a: Int, b: Int) -> String` for `range`, defined on
    /// first use. `a` and `b` are ignored for the whole array, `startIndex` and `endIndex` for a
    /// start-end range, and `offset` and `length` for an offset-length one.
    pub(crate) fn char_array_to_string(
        &mut self,
        module: &mut Module,
        source: &dyn BackendClassifierSource,
        range: CharArrayRange,
    ) -> Result<u32, String> {
        let slot = match range {
            CharArrayRange::Whole => 0,
            CharArrayRange::StartEnd => 1,
            CharArrayRange::OffsetLength => 2,
        };
        if let Some(function) = self.char_array_text[slot] {
            return Ok(function);
        }
        let (form, _) = self
            .array_form(module, Ty::array(Ty::Char))
            .ok_or("a char array without storage")?;
        let out_of_bounds = self.exception_class(
            module,
            source,
            RuntimeExceptionKind::IndexOutOfBounds.classifier(),
        )?;
        let illegal_argument = self.exception_class(
            module,
            source,
            RuntimeExceptionKind::IllegalArgument.classifier(),
        )?;
        let storage_type = ValType::nullable(form.storage);
        let result = ValType::reference(self.string);
        let ty = module.func_type(vec![storage_type, ValType::I32, ValType::I32], vec![result]);
        let (units, first, second, start, end, copy) = (0, 1, 2, 3, 4, 5);
        let mut code = Code::default();
        match range {
            CharArrayRange::Whole => {
                code.i32_const(0)
                    .local_set(start)
                    .local_get(units)
                    .array_len()
                    .local_set(end);
            }
            CharArrayRange::StartEnd => {
                code.local_get(first)
                    .local_set(start)
                    .local_get(second)
                    .local_set(end);
                // `startIndex < 0 || endIndex > size` is out of bounds; past that, an inverted
                // range is an illegal argument.
                code.local_get(start)
                    .i32_const(0)
                    .op(LT_S32)
                    .local_get(end)
                    .local_get(units)
                    .array_len()
                    .op(GT_S32)
                    .op(OR32)
                    .if_(BlockType::Empty);
                self.throw_new(&mut code, out_of_bounds)?;
                code.local_get(start)
                    .local_get(end)
                    .op(GT_S32)
                    .if_(BlockType::Empty);
                self.throw_new(&mut code, illegal_argument)?;
            }
            CharArrayRange::OffsetLength => {
                code.local_get(first)
                    .local_set(start)
                    .local_get(first)
                    .local_get(second)
                    .op(ADD32)
                    .local_set(end);
                // `offset < 0 || length < 0 || offset > size || length > size - offset`, which
                // cannot overflow where `offset + length` could.
                code.local_get(first)
                    .i32_const(0)
                    .op(LT_S32)
                    .local_get(second)
                    .i32_const(0)
                    .op(LT_S32)
                    .op(OR32)
                    .local_get(first)
                    .local_get(units)
                    .array_len()
                    .op(GT_S32)
                    .op(OR32)
                    .local_get(second)
                    .local_get(units)
                    .array_len()
                    .local_get(first)
                    .op(SUB32)
                    .op(GT_S32)
                    .op(OR32)
                    .if_(BlockType::Empty);
                self.throw_new(&mut code, out_of_bounds)?;
            }
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
            .array_copy(self.chars, form.storage);
        code.local_get(copy).struct_new(self.string);
        let locals = vec![ValType::I32, ValType::I32, ValType::reference(self.chars)];
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        self.char_array_text[slot] = Some(function);
        Ok(function)
    }

    /// Throw a new instance of `class` and close the `if` this opened.
    fn throw_new(&self, code: &mut Code, class: u32) -> Result<(), String> {
        self.exceptions
            .construct_class(code, class, None, self.string)?;
        code.throw(self.exceptions.tag).end();
        Ok(())
    }
}
