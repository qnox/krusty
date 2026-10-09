//! The Kotlin Toolchain project model, read the way JetBrains' `kotlin` command reads it.
//!
//! The public surface is what a command needs: read the [`model`] and its modules'
//! [`configuration`], report their [`diagnostic`]s and [`show`] them. Discovery, the file system
//! boundary, YAML, the schema and the build files' readers are this crate's own.

pub mod configuration;
pub mod diagnostic;
pub mod model;
pub mod show;

mod glob;
mod inventory;
mod maven_version;
mod module;
mod project;
mod reading;
mod schema;
mod tree;
mod yaml;

// The references the unit tests compare with, shared with the integration tests.
#[cfg(test)]
#[path = "../tests/support/jdk_globs.rs"]
mod jdk_globs;
#[cfg(test)]
#[path = "../tests/support/oracle.rs"]
mod oracle;
