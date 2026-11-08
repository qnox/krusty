//! JavaScript backend.

pub mod backend;
mod control_flow;
mod emit;
mod provider_operations;

pub use backend::JsBackend;
pub use emit::emit_file;
