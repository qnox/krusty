//! `task :<module>:compileJvm…`: compile modules' main fragments, and the main fragments of the
//! modules their compilation needs, with krusty, passing it the arguments JetBrains' toolchain
//! passes the Kotlin compiler.
//!
//! The whole plan is built before anything is compiled: every module's compiler arguments, and
//! every refusal of what krusty-toolchain does not implement (toolchain plugins other than ones
//! that add source directories, KSP, Java sources, compiler settings it does not pass, a JDK it
//! would have to provision, an artifact it would have to download). Then the modules are compiled, several at once, each after the
//! modules it compiles against, starting no more once one fails.

mod arguments;
mod classpath;
mod jdk;
mod order;
mod plan;
mod plugins;
mod refusals;
mod run;
mod schedule;
mod sources;

pub use plan::{plan, Environment, Plan, Step};
pub use run::{default_jobs, run, Printed, RunFailure};

/// The only task krusty-toolchain runs, as the toolchain names it after the module.
pub const TASK: &str = "compileJvm";
