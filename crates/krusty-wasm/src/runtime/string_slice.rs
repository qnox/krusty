//! `String.subSequence(Int, Int)`: the characters of a string between two indices, itself a string.
//!
//! One function checks the range as Kotlin does, throwing `IndexOutOfBoundsException` unless
//! `0 <= start <= end <= length`, then copies the units into a new array; a module carries it only
//! once a call needs it. A direct call, the `CharSequence` dispatch entry of a string and that of
//! the runtime's `StringBuilder` all reach it, so no route can skip the check.

use super::*;
use krusty::backend::BackendClassifierSource;
use krusty::libraries::RuntimeExceptionKind;

const GE_U32: u8 = 0x4f;

impl Runtime {
    /// The slice function, once a call or a `CharSequence` implementation has defined it.
    pub(crate) fn defined_string_sub_sequence(&self) -> Option<u32> {
        self.string_sub_sequence
    }

    /// The range-checked copy of units in use, once a call or an implementation has defined it.
    pub(crate) fn defined_units_sub_sequence(&self) -> Option<u32> {
        self.units_sub_sequence
    }

    /// The function `(units: chars, length: Int, start: Int, end: Int) -> String` copying
    /// `units[start until end]` of the `length` units in use, defined on first use.
    pub(crate) fn units_sub_sequence(
        &mut self,
        module: &mut Module,
        source: &dyn BackendClassifierSource,
    ) -> Result<u32, String> {
        if let Some(function) = self.units_sub_sequence {
            return Ok(function);
        }
        let exception = self.exception_class(
            module,
            source,
            RuntimeExceptionKind::IndexOutOfBounds.classifier(),
        )?;
        let chars = ValType::reference(self.chars);
        let result = ValType::reference(self.string);
        let ty = module.func_type(
            vec![chars, ValType::I32, ValType::I32, ValType::I32],
            vec![result],
        );
        let (units, length, start, end, copy) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        // `end <= length`, then `start <= end`; an unsigned comparison also rejects a negative
        // index, and the first check keeps `end + 1` from overflowing the second.
        for check in 0..2 {
            if check == 0 {
                code.local_get(end).local_get(length);
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
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: vec![chars],
                code,
            },
        );
        self.units_sub_sequence = Some(function);
        Ok(function)
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
        let slice = self.units_sub_sequence(module, source)?;
        // The receiver may be carried nullable; the checked call proved it is not null.
        let receiver = self.nullable_string_type();
        let result = ValType::reference(self.string);
        let ty = module.func_type(vec![receiver, ValType::I32, ValType::I32], vec![result]);
        let (string, start, end, units) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.local_get(string)
            .struct_get(self.string, 0)
            .local_set(units);
        code.local_get(units)
            .local_get(units)
            .array_len()
            .local_get(start)
            .local_get(end)
            .call(slice);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: vec![ValType::reference(self.chars)],
                code,
            },
        );
        self.string_sub_sequence = Some(function);
        Ok(function)
    }
}
