//! Native backend — Kotlin to a self-contained executable, with no JVM in the result.
//!
//! `docs/BUILD_AND_NATIVE_PLAN.md` argues that the opening against Kotlin/Native is *build speed*,
//! not generated-code quality: Kotlin/Native's LLVM pipeline is slow, and a target that accepts
//! weaker optimization in exchange for fast builds is worth having. This module is the first step
//! of that track (phase 8 in the plan) — it emits C, so the first native programs exist before the
//! code-generator decision has to be made.
//!
//! What is deliberately NOT here yet, and is tracked in the plan rather than stubbed:
//!
//! * **klib ingestion** (phase 7). Symbols still come from the Kotlin/JVM stdlib jar, because that
//!   is the only provider krusty has. Only *signatures* come from there; the emitted program links
//!   against `krusty_rt.c` and nothing else.
//! * **Interfaces and objects.** Classes with fields, methods, properties, single inheritance,
//!   virtual dispatch and `is`/`as` live on the heap (`classes.rs` lays them out and builds their
//!   vtables, `emit.rs` constructs them and dispatches through `kotlin.Any`-rooted type
//!   descriptors); interface dispatch and `object` declarations decline.
//! * **Most of the stdlib, and data classes, enums, lambdas.** Every one of them makes the backend
//!   decline with a diagnostic naming the construct, rather than emit something unverified.

pub mod backend;
mod classes;
mod emit;
mod gc;
mod intrinsics;
mod link;
mod runtime;
mod target;

pub use backend::{
    NativeBackend, ENTRY_SOURCE, GC_SOURCE, RUNTIME_HEADER, RUNTIME_SOURCE, START_SOURCE,
    SYS_HEADER,
};
pub use link::{c_compiler, can_build, link_executable, LinkError};
pub use target::{Arch, NativeTarget, Os};
