//! Build-tool project model used by the language server.

pub mod bsp;
pub mod detect;
pub mod fingerprint;
pub mod gradle;
pub mod jdk;
pub mod jps;
mod lock;
pub mod maven;
pub mod model;
pub mod provider;
pub mod runner;
mod sources;
pub mod sync;
mod walk;
mod xml;

#[cfg(test)]
mod testing;
#[cfg(test)]
pub(crate) use testing::TempTree;

pub use bsp::{discover as discover_bsp, BspConnection, BspProvider};
pub use detect::{detect, find_build_root};
pub use jdk::{
    is_jdk_home, resolve_jdk, Jdk, JdkEnvironment, JdkRequest, JdkSource, SystemEnvironment,
};
pub use model::{Module, ModuleId, ProjectModel, ProviderKind, SourceRoot, SourceRootKind};
pub use provider::{ProbeError, ProjectProvider};
pub use runner::{Command, CommandOutput, CommandRunner, ProcessRunner};
pub use sources::{workspace_sources, LoadedProjectSources, ProjectSources};
pub use sync::{ProjectSync, RefreshOutcome};
