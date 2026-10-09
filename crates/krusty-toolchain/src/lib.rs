//! The Kotlin Toolchain project model, read the way JetBrains' `kotlin` command reads it.
//!
//! The public surface is what a command needs: read the [`model`], report its [`diagnostic`]s and
//! [`show`] it. Discovery, the file system boundary, YAML and the build files' readers are this
//! crate's own.

pub mod diagnostic;
pub mod model;
pub mod show;

mod glob;
mod inventory;
mod module;
mod project;
mod reading;
mod yaml;

// The references the unit tests compare with, shared with the integration tests.
#[cfg(test)]
#[path = "../tests/support/jdk_globs.rs"]
mod jdk_globs;
#[cfg(test)]
#[path = "../tests/support/oracle.rs"]
mod oracle;
