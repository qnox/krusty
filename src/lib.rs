//! krusty — a memory-lean Kotlin→JVM compiler PoC.
//!
//! The pipeline is intentionally *linear and per-file streaming*: signatures are collected
//! globally (cheap), then each file is typechecked → lowered → emitted → dropped, so the working
//! set is bounded by a single file rather than the whole-module IR graph that makes kotlinc's
//! memory scale with module size. See `docs/SPEC.md`.

pub mod cli;
pub mod diag;
pub mod token;
pub mod lexer;
pub mod ast;
pub mod parser;
pub mod types;
pub mod resolve;
pub mod ir;
pub mod metadata;
pub mod backend;
pub mod jvm;
