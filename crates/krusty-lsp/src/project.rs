//! Build-tool project model used by the language server.

pub mod bsp;
pub mod detect;
pub mod fingerprint;
pub mod gradle;
pub mod jdk;
pub mod maven;
pub mod model;
pub mod provider;
pub mod runner;
pub mod sync;
mod xml;

#[cfg(test)]
mod testing;

pub use bsp::{discover as discover_bsp, BspConnection, BspProvider};
pub use detect::{detect, find_build_root};
pub use jdk::{
    is_jdk_home, resolve_jdk, Jdk, JdkEnvironment, JdkRequest, JdkSource, SystemEnvironment,
};
pub use model::{Module, ModuleId, ProjectModel, ProviderKind, SourceRoot, SourceRootKind};
pub use provider::{ProbeError, ProjectProvider};
pub use runner::{Command, CommandOutput, CommandRunner, ProcessRunner};
pub use sync::{ProjectSync, RefreshOutcome};
