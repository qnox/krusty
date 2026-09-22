//! Build layer for krusty — the skeleton of the Go-like build model in
//! the build-layer contract in `docs/ARCHITECTURE.md`.
//!
//! `go build` is fast because of per-package units, compact export data, a content-addressed cache
//! keyed on inputs plus dependency ABI hashes, and a parallel DAG over a compiler that starts
//! instantly. krusty already supplies the last of those. This crate is where the rest goes.
//!
//! # What is here
//!
//! * [`model`] — the shape of a module. Deliberately WIDER than the language server's
//!   `ProjectModel`, which omits several things a build needs (see its module docs).
//! * [`graph`] — the module DAG: deterministic topological order, cycle detection, and the
//!   transitive-dependents query that decides what a changed ABI invalidates.
//! * [`digest`] — the hasher everything else is keyed on, and the one place the hash construction
//!   is written down.
//!
//! # What is NOT here YET
//!
//! The ABI model and its fingerprint, the cache key, the artifact store, and the driver that ties
//! them together. Each lands on top of this one, in that order, because that is the order they
//! depend on each other: an ABI fingerprint is a [`digest`], a cache key folds in ABI
//! fingerprints, the store is keyed by cache key, and the driver walks the [`graph`] through all
//! three.
//!
//! No build providers (Gradle/Maven/BSP/JPS) and no parallel scheduling either. The providers are
//! ~10,000 lines living in `crates/krusty-lsp/src/project/` and lifting them is its own change;
//! this crate establishes the types they will populate. Parallelism needs a process pool with
//! crash isolation and interleaved diagnostics, and a sequential driver is its oracle: the same
//! graph must produce the same artifacts either way.
//!
//! Nothing here is wired into the compiler or the shipped CLI yet. The crate builds and tests
//! standalone.
//!
//! # The property this all rests on
//!
//! Emission must be deterministic, or a content-addressed cache *inverts*: an unchanged module's
//! hash changes on rebuild, so every dependent rebuilds rather than none — and the cache then hides
//! the defect, because a nondeterminism bug that would surface as a byte diff instead surfaces as a
//! cache hit. `tests/emission_determinism_e2e.rs` gates that property.

pub mod digest;
pub mod graph;
pub mod model;

pub use digest::{digest_bytes, Digest, Hasher};
pub use graph::{GraphError, ModuleGraph};
pub use model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};
