//! krusty — a memory-lean Kotlin→JVM compiler PoC.
//!
//! The pipeline is intentionally *linear and per-file streaming*: signatures are collected
//! globally (cheap), then each file is typechecked → lowered → emitted → dropped, so the working
//! set is bounded by a single file rather than the whole-module IR graph that makes kotlinc's
//! memory scale with module size. See `docs/SPEC.md`.

// Re-exported under the `dhat-heap` feature so the integration-test crate can name dhat's global
// allocator (`krusty::dhat::Alloc`) without a separate dev-dependency. Not compiled otherwise.
#[cfg(feature = "dhat-heap")]
pub use dhat;

pub mod assignable;
pub mod ast;
pub mod backend;
pub mod cli;
pub mod compiler;
pub mod conformance;
pub mod diag;
pub mod features;
pub mod frontend;
pub mod ir;
pub mod ir_lower;
pub mod js;
pub mod jvm;
pub mod lexer;
pub mod libraries;
pub mod lru;
pub mod lsp;
pub mod metadata;
pub mod module_symbols;
pub mod names;
pub mod parser;
pub mod plugins;
mod resolve;
pub mod runtime;
pub mod symbol_resolver;
pub mod symbol_source;
pub mod synthetics;
pub mod token;
pub mod toolchain;
pub mod trace;
pub mod types;

#[cfg(test)]
mod architecture;
