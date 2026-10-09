//! The Kotlin Toolchain project model, read the way JetBrains' `kotlin` command reads it.

pub mod configuration;
pub mod diagnostic;
pub mod glob;
pub mod inventory;
pub mod maven;
pub mod model;
pub mod module;
pub mod project;
pub mod reading;
pub mod resolution;
pub mod schema;
pub mod show;
pub mod tree;
pub mod yaml;
