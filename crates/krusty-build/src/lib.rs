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
//! * [`abi`] — an ABI model carrying no method bodies, and its fingerprint. This is the artifact a
//!   dependent compiles against and the hash every dependent's cache key folds in.
//! * [`cache`] — the cache key. Every input that can change emitted bytes is in it by default.
//! * [`store`] — the content-addressed artifact store. Its one job beyond lookup is integrity: a
//!   partially written entry must never read as a hit.
//!
//! # What is NOT here YET
//!
//! The driver that ties everything together, and the build environment that runs the real
//! compiler under it. Both land on top of this one: the driver walks the [`graph`] through all of
//! the above.
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

pub mod abi;
pub mod cache;
pub mod digest;
pub mod graph;
pub mod model;
pub mod store;

pub use abi::{AbiClass, AbiFingerprint, AbiMember, MemberKind};
pub use cache::{CacheKey, CacheKeyInputs, FileDigest};
pub use digest::{digest_bytes, Digest, Hasher};
pub use graph::{GraphError, ModuleGraph};
pub use model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};
pub use store::{ArtifactStore, CachedModule, MissReason};
