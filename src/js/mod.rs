//! JavaScript backend.

pub mod backend;
mod control_flow;
mod emit;

pub use backend::JsBackend;
pub use emit::emit_file;
