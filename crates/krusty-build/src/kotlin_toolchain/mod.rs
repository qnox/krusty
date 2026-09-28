//! Kotlin Toolchain projects (`module.yaml`, `project.yaml`) as a `kotlin build` input.
//!
//! `krusty` remains a drop-in for `kotlinc`, in its own executable. This module is what `kotlin build`
//! loads for JVM modules. Gradle and JetBrains `.iml` projects are recognized and refused here;
//! their project-model extensions stay in the language server.

mod discover;
mod load;
mod yaml;

pub use load::{execute, load, BuildCommand, LoadedProject};
