//! WebAssembly backend — Kotlin to a WasmGC module, for kotlinc's two Wasm targets.
//!
//! `wasm-js` and `wasm-wasi` share everything but the host boundary (see [`target`]): the same
//! object model on the GC proposal's structs and arrays, the same instruction selection, the same
//! runtime emitted into the module. The pipeline is
//!
//! checked common IR → codegen (per file, into one module) → runtime + host imports → `.wasm`,
//! plus the `.mjs` loader that runs it under Node.js.
//!
//! A Wasm module is linked as a whole: a function's index is its address, and the runtime's types
//! are the program's types. So files are lowered into one growing module and the module is encoded
//! once, when the backend is finalized.

mod codegen;
mod encode;
mod host;
mod objects;
mod runtime;
mod target;

pub use codegen::WasmBackend;
pub use target::WasmTarget;
