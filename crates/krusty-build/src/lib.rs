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
//! * [`driver`] — plan, look up, compile, store, materialize. Sequential, because the compiler is
//!   not `Send`.
//! * [`compiler`] — a [`driver::BuildEnvironment`] that drives the real `krusty` binary, one
//!   process per module.
//! * [`gradle`] — runs a project's Gradle wrapper with the typed `krusty` plugin, either from this
//!   checkout or from its release Maven repository. Kotlin/JVM continues to own the source model;
//!   the replacement compilation task execs krusty and does not call back into this crate.
//! * [`kotlin_toolchain`] — `module.yaml` / `project.yaml` loading for `krusty-toolchain build`. The
//!   executable lives in `krusty-toolchain`; this crate owns its process-independent project model.
//!
//! # What is NOT here
//!
//! Maven, BSP, and JPS remain language-server project models. The toolchain loader recognizes a
//! Gradle or `.iml` tree and stops rather than treating it as a toolchain project. Parallelism needs
//! a process pool with crash isolation and interleaved diagnostics, and the sequential driver is
//! its oracle: the same graph must produce the same artifacts either way.
//!
//! # The property this all rests on
//!
//! Emission must be deterministic, or a content-addressed cache *inverts*: an unchanged module's
//! hash changes on rebuild, so every dependent rebuilds rather than none — and the cache then hides
//! the defect, because a nondeterminism bug that would surface as a byte diff instead surfaces as a
//! cache hit. `tests/emission_determinism_e2e.rs` gates that property.

pub mod abi;
pub mod cache;
pub mod compiler;
pub mod digest;
pub mod driver;
pub mod gradle;
pub mod graph;
pub mod kotlin_toolchain;
pub mod model;
pub mod store;

pub use abi::{
    AbiAnnotation, AbiAnnotationValue, AbiClass, AbiConstant, AbiFingerprint, AbiMember, MemberKind,
};
pub use cache::{CacheKey, CacheKeyInputs, FileDigest};
pub use compiler::KrustyCli;
pub use digest::{digest_bytes, Digest, Hasher};
pub use driver::{BuildEnvironment, BuildReport, CompiledModule, Driver, Outcome, PublishedAbi};
pub use gradle::{repository_plugin_project, GradleBuild, GradleError};
pub use graph::{GraphError, ModuleGraph};
pub use model::{Module, ModuleId, ModuleOutput, SourceRoot, SourceRootKind};
pub use store::{ArtifactStore, CachedModule, MissReason, StoreLookup};
