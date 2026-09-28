//! Kotlin Toolchain projects (`module.yaml`, `project.yaml`) as a `krusty-toolchain build` input.
//!
//! `krusty` remains a drop-in for `kotlinc`, in its own executable. This module is what
//! `krusty-toolchain build` loads for JVM modules. Gradle and JetBrains `.iml` projects are recognized
//! and refused here; their project-model extensions stay in the language server.

mod catalog;
mod discover;
mod load;
mod yaml;

pub use load::{execute, load, BuildCommand, LoadedProject};
