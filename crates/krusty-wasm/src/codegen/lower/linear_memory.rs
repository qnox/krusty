//! Calls of target-stdlib declarations whose implementation is one linear-memory instruction
//! (`wasm_i32_load`, `wasm_i32_store8`, `wasm_memory_grow` …), emitted as that instruction on the
//! module's memory.

use super::*;
use krusty::libraries::LinearMemoryOperation;

impl Body<'_> {
    /// Emit the operands of a call of `memory`, whose declaration the provider authenticated as
    /// `params` to `ret`, then the instruction.
    pub(super) fn linear_memory_call(
        &mut self,
        memory: LinearMemoryOperation,
        params: &[Ty],
        ret: Ty,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        if !memory.admits(params, ret) || args.len() != params.len() {
            return Err(format!(
                "a call of the linear-memory instruction {memory:?} with parameters {} and result {}",
                params
                    .iter()
                    .map(|ty| describe(*ty))
                    .collect::<Vec<_>>()
                    .join(", "),
                describe(ret)
            ));
        }
        for (arg, ty) in args.iter().zip(params) {
            self.value(*arg, *ty)?;
        }
        match memory {
            LinearMemoryOperation::Load { bytes } => {
                let (opcode, align) = match bytes {
                    1 => (0x2c, 0),
                    2 => (0x2e, 1),
                    4 => (0x28, 2),
                    _ => (0x29, 3),
                };
                self.code.memory_access(opcode, align);
            }
            LinearMemoryOperation::Store { bytes } => {
                let (opcode, align) = match bytes {
                    1 => (0x3a, 0),
                    2 => (0x3b, 1),
                    4 => (0x36, 2),
                    _ => (0x37, 3),
                };
                self.code.memory_access(opcode, align);
            }
            LinearMemoryOperation::Size => {
                self.code.memory_size();
            }
            LinearMemoryOperation::Grow => {
                self.code.memory_grow();
            }
        }
        Ok(())
    }
}
