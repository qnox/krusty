//! Kotlin Toolchain projects (`module.yaml`, `project.yaml`) as a `krusty-toolchain build` input.
//!
//! `krusty` remains a drop-in for `kotlinc`, in its own executable. This module is what
//! `krusty-toolchain build` loads for JVM modules. Gradle and Maven projects are compiled by running
//! those tools. A JetBrains `.iml` project is recognized and refused; that project-model extension
//! stays in the language server.

mod catalog;
mod discover;
mod gradle_model;
mod load;
mod maven_model;
mod tool;
mod yaml;

pub use load::{execute, load, BuildCommand, LoadedProject};
