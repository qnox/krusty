//! Kotlin Toolchain projects (`module.yaml`, `project.yaml`) as a `krusty-toolchain build` input.
//!
//! `krusty` remains a drop-in for `kotlinc`, in its own executable. This module is what
//! `krusty-toolchain build` loads for JVM modules described by `module.yaml` and `project.yaml`.
//! Gradle, Maven, and `.iml` dependency models stay in the language server. A Gradle, Maven, or
//! Bazel build compiles through its own plugin.

mod catalog;
mod discover;
mod load;
mod yaml;

pub use load::{execute, load, BuildCommand, LoadedProject};
