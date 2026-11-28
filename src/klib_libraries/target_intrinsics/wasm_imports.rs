//! Kotlin/Wasm host imports: the bodyless `@WasmImport` declarations a target stdlib binds to its
//! host's functions.

use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};

use super::ir_annotation_arguments::ir_string_annotation_arguments;

/// The host function a `@WasmImport` declaration names.
pub(crate) struct WasmImport {
    pub(crate) module: String,
    pub(crate) name: String,
}

/// Kotlin/Wasm's `@WasmImport(module, name)` on a bodyless `external` function: the host function
/// kotlinc imports for it under `module` and `name`, the declaration's own name when `name` is
/// left out or empty. The annotation class's public identity is the whole contract, so the
/// declaration may be file-local, as the WASI stdlib's private `fd_write` binding is.
pub(crate) fn wasm_import(arena: &KlibIrArena, function: &KlibIrFunction) -> Option<WasmImport> {
    if function.body.is_some()
        || function.constructor
        || function.dispatch_receiver.is_some()
        || function.extension_receiver.is_some()
        || !function.context_parameters.is_empty()
        || !function.type_parameters.is_empty()
    {
        return None;
    }
    let arguments = ir_string_annotation_arguments(
        arena,
        &function.base.annotations,
        &["kotlin", "wasm"],
        &["WasmImport", "<init>"],
    )?;
    let (module, name) = match arguments.as_slice() {
        [Some(module)] | [Some(module), None] => (*module, ""),
        [Some(module), Some(name)] => (*module, *name),
        _ => return None,
    };
    Some(WasmImport {
        module: module.to_owned(),
        name: if name.is_empty() {
            function.name.clone()
        } else {
            name.to_owned()
        },
    })
}
