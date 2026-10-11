//! krusty's cinterop: C headers to a Kotlin/Native-compatible interop library.
//!
//! Kotlin/Native's `cinterop` tool reads a `.def` file and the C headers it names, and writes a
//! klib whose metadata declares the C functions, structs, enums, typedefs and constant macros as
//! Kotlin declarations. krusty does the same without a C compiler: it preprocesses and parses the
//! headers itself, computes every layout for the target's ABI, and writes the declarations in
//! Kotlin/Native's direct-call format. See `docs/NATIVE.md`, "C interop".

mod compiler_headers;
mod declarations;
mod def_file;
mod layout;
mod lexer;
mod library;
mod link_facts;
mod model;
mod preprocessor;
mod tool;

pub use library::CinteropError;
pub(super) use link_facts::imported as imported_libraries;
pub use tool::{build_interop_library, CinteropOptions};
