//! `task :<module>:compileJvm`: compile a module's main fragment, and the main fragments of the
//! modules its compilation needs, with krusty, passing it the arguments JetBrains' toolchain passes
//! the Kotlin compiler.
//!
//! The whole plan is built before anything is compiled: every module's compiler arguments, and
//! every refusal of what krusty-toolchain does not implement (toolchain plugins, KSP, Java
//! sources, compiler settings it does not pass, a JDK it would have to provision, an artifact it
//! would have to download). Then the modules are compiled in dependency order, stopping at the
//! first that fails.

mod arguments;
mod classpath;
mod jdk;
mod order;
mod plan;
mod refusals;
mod run;
mod sources;

pub use plan::{plan, Environment, Plan, Step};
pub use run::{run, RunFailure};

/// The only task krusty-toolchain runs, as the toolchain names it after the module.
pub const TASK: &str = "compileJvm";
