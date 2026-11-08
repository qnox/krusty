//! Which input of a compilation an emitted artifact belongs to.
//!
//! Pass 2 lowers and emits one source file at a time, so each per-file artifact has exactly one
//! source. A caller that keeps output between compilations (an incremental driver deleting the
//! classes of a recompiled file) needs that mapping. It cannot recover it from class names: a
//! file's lambda, local and nested classes, and its `@JvmName` facade, are named by the backend.

/// The input an artifact was emitted for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ArtifactOrigin {
    /// One source file, by its position in the analyzed source list.
    Source(usize),
    /// Target support for a dependency declaration, emitted while lowering the source at this
    /// position because that source uses it first.
    DependencySupport { requested_by: usize },
    /// The module as a whole, from `Backend::finalize`: an artifact assembled from several sources
    /// (a multifile facade) or describing the module (its `.kotlin_module`).
    Module,
}
