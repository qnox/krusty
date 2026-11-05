//! Kotlin String hashing over the runtime's complete UTF-16 code-unit storage.

use super::*;

impl Runtime {
    pub(super) fn define_string_hash(&self, module: &mut Module) {
        let ty = module.func_type(vec![self.string_type()], vec![ValType::I32]);
        let (value, chars, index, hash) = (0, 1, 2, 3);
        let mut code = Code::default();
        code.local_get(value)
            .struct_get(self.string, 0)
            .local_set(chars);
        code.loop_(BlockType::Empty)
            .local_get(index)
            .local_get(chars)
            .array_len()
            .op(GE_U32)
            .if_(BlockType::Empty)
            .local_get(hash)
            .return_()
            .end()
            .local_get(hash)
            .i32_const(31)
            .op(MUL32)
            .local_get(chars)
            .local_get(index)
            .array_get_u(self.chars)
            .op(ADD32)
            .local_set(hash)
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .unreachable();
        module.define(
            self.string_hash,
            Function {
                ty,
                locals: vec![ValType::reference(self.chars), ValType::I32, ValType::I32],
                code,
            },
        );
    }
}
