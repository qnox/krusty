//! krusty — a memory-lean Kotlin→JVM compiler PoC.
//!
//! The pipeline is intentionally *linear and per-file streaming*: signatures are collected
//! globally (cheap), then each file is typechecked → lowered → emitted → dropped, so the working
//! set is bounded by a single file rather than the whole-module IR graph that makes kotlinc's
//! memory scale with module size. See `docs/SPEC.md`.

pub mod ast;
pub mod backend;
pub mod cli;
pub mod diag;
pub mod ir;
pub mod ir_lower;
pub mod js;
pub mod jvm;
pub mod lexer;
pub mod libraries;
pub mod metadata;
pub mod parser;
pub mod resolve;
pub mod token;
pub mod types;
