//! Bodyless `@WasmImport` dependency functions, which a unit declares without a body and the
//! target imports from its host (`IrFile::host_imports`).

use super::super::decline::KlibBodyDeclineReason;
use super::super::function_lowering::{declare_function, FunctionHeader};
use crate::ir::{FunId, IrFile, IrHostImport};
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};

/// Require the body that declaring `function` lowers; a host function has none to lower.
pub(super) fn require_body(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<(), KlibBodyDeclineReason> {
    if crate::klib_libraries::wasm_import(arena, function).is_none() {
        super::block_statements(function)?;
    }
    Ok(())
}

/// Declare `function` as the host function it imports, when it is one.
pub(super) fn declare(
    ir: &mut IrFile,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    header: &FunctionHeader,
) -> Option<FunId> {
    let import = crate::klib_libraries::wasm_import(arena, function)?;
    let declared = declare_function(ir, header);
    ir.host_imports.insert(
        declared,
        IrHostImport {
            module: import.module,
            name: import.name,
        },
    );
    Some(declared)
}
