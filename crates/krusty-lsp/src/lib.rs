mod analysis;
mod compiler_analysis;
mod dependency_sources;
mod dependency_symbols;
pub mod deps_cache;
pub mod dump_cache;
mod options;
pub mod project;
mod server;
pub mod uri;
mod worker;

pub use analysis::*;
pub use compiler_analysis::LibraryRef;
pub use dependency_sources::render as deps_render;
pub use dependency_symbols::{DependencyCandidate, DependencySymbolIndex, MAX_DEPENDENCY_CLASSES};
pub use options::*;
pub use project::{
    detect, resolve_jdk, JdkRequest, LoadedProjectSources, ProcessRunner, ProjectModel,
    ProjectSources, ProjectSync, ProviderKind, RefreshOutcome, SystemEnvironment,
};
pub use server::*;
pub use worker::*;

/// Progress of the workspace file-tree scan that feeds background indexing. The scan walks every
/// module source root on the engine thread, which can take minutes on a large tree; these events
/// are what the client sees while it runs.
pub enum ScanProgress {
    /// The walk over the project model's source roots has begun.
    Started,
    /// Checkpoint mid-walk: `files` sources discovered so far.
    Found { files: u64 },
    /// The walk finished: `files` sources discovered in `millis`.
    Finished { files: u64, millis: u64 },
}

/// Consumer installed by the engine so the scanning backend can report without knowing the event
/// channel. `Send` because the reporter crosses into the backend that lives on the engine thread.
pub type ScanReporter = Box<dyn FnMut(ScanProgress) + Send>;
