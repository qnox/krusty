//! Linear-memory instructions a target stdlib declares as its own implementation.
//!
//! Kotlin/Wasm spells its raw memory access (`wasm_i32_load`, `wasm_i32_store8`,
//! `wasm_memory_size` …) as `@WasmOp` declarations whose whole implementation is one memory
//! instruction on the module's linear memory. The provider authenticates the declaration's exact
//! parameter and result types against the instruction it names; only the target emitter replaces
//! a call of it with the instruction.

use crate::types::Ty;

/// One linear-memory instruction. Addresses are `Int` byte offsets into memory 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinearMemoryOperation {
    /// Read `bytes` bytes at an address into the declaration's result, sign-extending a
    /// narrower read.
    Load { bytes: u8 },
    /// Write the low `bytes` bytes of its value parameter at an address.
    Store { bytes: u8 },
    /// The memory's current size in 64 KiB pages.
    Size,
    /// Grow the memory by a number of pages, returning its previous size or `-1`.
    Grow,
}

impl LinearMemoryOperation {
    /// The operation a `WasmOp` entry names, when its declaration has exactly the instruction's
    /// operand and result types.
    pub fn of_wasm_op(name: &str, params: &[Ty], ret: Ty) -> Option<Self> {
        let operation = match name {
            "I32_LOAD8_S" => Self::Load { bytes: 1 },
            "I32_LOAD16_S" => Self::Load { bytes: 2 },
            "I32_LOAD" => Self::Load { bytes: 4 },
            "I64_LOAD" => Self::Load { bytes: 8 },
            "I32_STORE8" => Self::Store { bytes: 1 },
            "I32_STORE16" => Self::Store { bytes: 2 },
            "I32_STORE" => Self::Store { bytes: 4 },
            "I64_STORE" => Self::Store { bytes: 8 },
            "MEMORY_SIZE" => Self::Size,
            "MEMORY_GROW" => Self::Grow,
            _ => return None,
        };
        operation.admits(params, ret).then_some(operation)
    }

    /// Whether the instruction is defined over `params` producing `ret`: an `Int` address and the
    /// value type its access width fixes. Authentication and target emission share this rule.
    pub fn admits(self, params: &[Ty], ret: Ty) -> bool {
        match self {
            Self::Load { bytes } => params == [Ty::Int] && value_type(bytes) == Some(ret),
            Self::Store { bytes } => {
                ret == Ty::Unit
                    && matches!(params, [Ty::Int, value] if value_type(bytes) == Some(*value))
            }
            Self::Size => params.is_empty() && ret == Ty::Int,
            Self::Grow => params == [Ty::Int] && ret == Ty::Int,
        }
    }
}

/// The Kotlin type a memory access of `bytes` bytes reads or writes.
fn value_type(bytes: u8) -> Option<Ty> {
    Some(match bytes {
        1 => Ty::Byte,
        2 => Ty::Short,
        4 => Ty::Int,
        8 => Ty::Long,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::LinearMemoryOperation;
    use crate::types::Ty;

    #[test]
    fn a_memory_instruction_requires_its_exact_operand_and_result_types() {
        assert_eq!(
            LinearMemoryOperation::of_wasm_op("I32_LOAD8_S", &[Ty::Int], Ty::Byte),
            Some(LinearMemoryOperation::Load { bytes: 1 })
        );
        assert_eq!(
            LinearMemoryOperation::of_wasm_op("I32_STORE8", &[Ty::Int, Ty::Byte], Ty::Unit),
            Some(LinearMemoryOperation::Store { bytes: 1 })
        );
        assert_eq!(
            LinearMemoryOperation::of_wasm_op("MEMORY_GROW", &[Ty::Int], Ty::Int),
            Some(LinearMemoryOperation::Grow)
        );
        for (name, params, ret) in [
            ("I32_LOAD8_S", &[Ty::Int][..], Ty::Int),
            ("I32_STORE8", &[Ty::Int, Ty::Int][..], Ty::Unit),
            ("I64_LOAD", &[Ty::Long][..], Ty::Long),
            ("MEMORY_SIZE", &[Ty::Int][..], Ty::Int),
            ("I32_ADD", &[Ty::Int, Ty::Int][..], Ty::Int),
        ] {
            assert_eq!(LinearMemoryOperation::of_wasm_op(name, params, ret), None);
        }
    }
}
