//! JavaScript backend.

pub mod backend;
mod emit;

pub use backend::JsBackend;
pub use emit::emit_file;
