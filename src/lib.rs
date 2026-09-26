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
pub mod ast_print;
mod ast_validate;
pub mod backend;
mod callable_access;
pub mod compiler;
pub mod conformance;
mod context_parameters;
pub mod contracts;
mod declaration_validation;
pub mod diag;
pub mod diagnostic_wording;
pub mod dump;
pub mod features;
pub mod fir;
pub mod fir_lower;
pub mod frontend;
mod inline_parameter_modifier;
pub mod ir;
pub mod ir_print;
pub mod java_source;
pub mod js;
pub mod jvm;
pub mod klib;
pub mod kotlin_version;
pub mod kt_string;
pub mod lexer;
pub mod libraries;
pub mod lru;
pub mod metadata;
pub mod module_symbols;
pub mod name_tree;
pub mod names;
pub mod parser;
pub mod plugins;
mod resolve;
pub mod runtime;
pub mod source;
pub mod spelling;
pub mod symbol_resolver;
pub mod symbol_source;
pub mod synthetics;
pub mod token;
pub mod toolchain;
pub mod trace;
pub mod type_engine;
pub mod types;
pub(crate) mod value_classes;
mod wide_stack;

#[cfg(test)]
mod architecture;
