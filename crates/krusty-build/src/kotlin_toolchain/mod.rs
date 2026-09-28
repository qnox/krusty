//! Kotlin Toolchain projects (`module.yaml`, `project.yaml`) as a `krusty build` input.
//!
//! `krusty` remains a drop-in for `kotlinc`. This module is the drop-in for the Kotlin Toolchain
//! CLI command `kotlin build`, for JVM modules. Gradle and JetBrains `.iml` projects are recognized
//! and refused here; their project-model extensions stay in the language server.

mod discover;
mod load;
mod yaml;

pub use load::{execute, load, BuildCommand, LoadedProject};
