//! `String.subSequence(Int, Int)`: the characters of a string between two indices, itself a string.
//!
//! The function copies the units into a new array; a module carries it only once a call needs it.
//! Range checking belongs to the call site, which throws the Kotlin exception.

use super::*;

impl Runtime {
    /// The function `(s: String, start: Int, end: Int) -> String` copying `s[start until end]`,
    /// defined on first use.
    pub(crate) fn string_sub_sequence(&mut self, module: &mut Module) -> u32 {
        if let Some(function) = self.string_sub_sequence {
            return function;
        }
        // The receiver may be carried nullable; the checked call proved it is not null.
        let receiver = self.nullable_string_type();
        let result = ValType::reference(self.string);
        let ty = module.func_type(vec![receiver, ValType::I32, ValType::I32], vec![result]);
        let (string, start, end, units, copy) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(string)
            .struct_get(self.string, 0)
            .local_set(units);
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
        function
    }
}
