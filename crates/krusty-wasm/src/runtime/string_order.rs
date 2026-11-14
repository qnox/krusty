//! `String.compareTo(String)`: the lexicographic order of two strings' UTF-16 code units.
//!
//! The answer is the difference of the first two units that differ, or of the lengths when one
//! string is a prefix of the other, which is the value every target's `compareTo` returns and a
//! program may print. A module carries the function only once a call needs it.

use super::*;

impl Runtime {
    /// The function `(a: String, b: String) -> Int` comparing two strings, defined on first use.
    pub(crate) fn string_compare(&mut self, module: &mut Module) -> u32 {
        if let Some(function) = self.string_compare {
            return function;
        }
        // Either operand may be carried nullable; the checked call proved it is not null.
        let string = self.nullable_string_type();
        let ty = module.func_type(vec![string, string], vec![ValType::I32]);
        let (a, b, left, right, left_length, right_length, length, index, difference) =
            (0, 1, 2, 3, 4, 5, 6, 7, 8);
        let mut code = Code::default();
        code.local_get(a)
            .struct_get(self.string, 0)
            .local_tee(left)
            .array_len()
            .local_set(left_length);
        code.local_get(b)
            .struct_get(self.string, 0)
            .local_tee(right)
            .array_len()
            .local_set(right_length);
        // The units both strings have.
        code.local_get(left_length)
            .local_get(right_length)
            .op(LT_S32)
            .if_(BlockType::Value(ValType::I32))
            .local_get(left_length)
            .else_()
            .local_get(right_length)
            .end()
            .local_set(length);
        code.loop_(BlockType::Empty);
        code.local_get(index).local_get(length).op(GE_U32);
        code.if_(BlockType::Empty)
            .local_get(left_length)
            .local_get(right_length)
            .op(SUB32)
            .return_()
            .end();
        code.local_get(left)
            .local_get(index)
            .array_get_u(self.chars)
            .local_get(right)
            .local_get(index)
            .array_get_u(self.chars)
            .op(SUB32)
            .local_tee(difference)
            .if_(BlockType::Empty)
            .local_get(difference)
            .return_()
            .end();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0);
        code.end().unreachable();
        let chars = ValType::reference(self.chars);
        let mut locals = vec![chars, chars];
        locals.extend([ValType::I32; 5]);
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        self.string_compare = Some(function);
        function
    }
}
