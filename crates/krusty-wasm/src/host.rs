//! The host boundary: the one place `wasm-js` and `wasm-wasi` differ.
//!
//! A module reaches the outside world only through what it imports. Both hosts are given the same
//! narrow surface — "these bytes of linear memory are output" — so the runtime above this file is
//! written once. Under `wasm-js` that is one function the loader supplies; under `wasm-wasi` it is
//! WASI's own `fd_write`, wrapped so its iovec bookkeeping stays here.
//!
//! The JavaScript loader is an artifact of the compilation, as kotlinc's `<module>.mjs` is: it
//! instantiates the module and runs its entry, so `node <module>.mjs` runs the program under either
//! target.

use super::encode::{Code, Function, FunctionImport, Module, ValType};
use super::runtime::BUFFER;
use super::target::WasmTarget;

/// What the host supplies to the runtime.
pub(super) struct Host {
    /// `(length: i32) -> ()`: write `length` bytes staged at [`BUFFER`] to standard output.
    pub(super) flush: u32,
    target: WasmTarget,
    /// The imported function output goes through: the loader's `write(pointer, length)` under
    /// `wasm-js`, WASI's `fd_write` under `wasm-wasi`.
    import: u32,
}

impl Host {
    /// Declare the target's imports. Runs before any function is defined.
    pub(super) fn import(module: &mut Module, target: WasmTarget) -> Self {
        module.memory(1);
        match target {
            WasmTarget::Js => {
                let ty = module.func_type(vec![ValType::I32, ValType::I32], Vec::new());
                let write = module.import(FunctionImport {
                    module: "krusty",
                    name: "write",
                    ty,
                });
                Self {
                    // Declared now, defined by `declare` once the imports are complete.
                    flush: u32::MAX,
                    target,
                    import: write,
                }
            }
            WasmTarget::Wasi => {
                let ty = module.func_type(vec![ValType::I32; 4], vec![ValType::I32]);
                let fd_write = module.import(FunctionImport {
                    module: "wasi_snapshot_preview1",
                    name: "fd_write",
                    ty,
                });
                Self {
                    flush: u32::MAX,
                    target,
                    import: fd_write,
                }
            }
        }
    }

    /// Define `flush` over the target's import. Runs after every import is declared.
    pub(super) fn declare(&mut self, module: &mut Module) {
        self.flush = module.declare();
        let ty = module.func_type(vec![ValType::I32], Vec::new());
        let mut code = Code::default();
        match self.target {
            WasmTarget::Js => {
                code.i32_const(BUFFER).local_get(0).call(self.import);
            }
            WasmTarget::Wasi => {
                // iovec { base: BUFFER, length } at address 0, one of them, to fd 1; the count
                // written goes to address 8. Standard output takes the whole buffer from the hosts
                // krusty runs under; a short write would need a loop here.
                code.i32_const(0).i32_const(BUFFER).i32_store(0);
                code.i32_const(0).local_get(0).i32_store(4);
                code.i32_const(1)
                    .i32_const(0)
                    .i32_const(1)
                    .i32_const(8)
                    .call(self.import)
                    .drop_();
            }
        }
        module.define(
            self.flush,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// The JavaScript loader that runs `<module_name>.wasm` under this target with Node.js.
    pub(super) fn loader(&self, module_name: &str) -> String {
        let read = format!(
            "import fs from 'node:fs';\n\
             const bytes = fs.readFileSync(new URL('./{module_name}.wasm', import.meta.url));\n\
             const module = new WebAssembly.Module(bytes);\n"
        );
        match self.target {
            WasmTarget::Js => format!(
                "{read}let memory;\n\
                 const imports = {{\n  krusty: {{\n    write: (pointer, length) => {{\n      \
                 fs.writeSync(1, new Uint8Array(memory.buffer, pointer, length));\n    }},\n  }},\n}};\n\
                 const instance = new WebAssembly.Instance(module, imports);\n\
                 memory = instance.exports.memory;\n\
                 instance.exports._start();\n"
            ),
            WasmTarget::Wasi => format!(
                "import {{ WASI }} from 'node:wasi';\n{read}\
                 const wasi = new WASI({{ version: 'preview1', args: process.argv.slice(1), env: process.env }});\n\
                 const instance = new WebAssembly.Instance(module, wasi.getImportObject());\n\
                 wasi.start(instance);\n"
            ),
        }
    }
}
