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

mod array_access;
mod array_objects;
mod boxes;
mod builtin_instances;
mod builtin_members;
mod builtin_order;
mod callable_references;
mod class_tokens;
mod codegen;
mod encode;
mod exception_transport;
mod function_values;
mod hashing;
mod host;
mod identity;
mod interface_dispatch;
mod mutable_list;
mod numeric_conversions;
mod objects;
mod rendering;
mod runtime;
mod string_builders;
mod target;
mod type_objects;

pub use codegen::WasmBackend;
pub use target::WasmTarget;
