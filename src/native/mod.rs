//! Native backend — Kotlin to a self-contained executable, with no JVM in the result and no other
//! compiler in the loop.
//!
//! `docs/BUILD_AND_NATIVE_PLAN.md` records the decisions this module implements, in order: krusty
//! owns its runtime; krusty owns its code generator, so it emits no C; Cranelift owns instruction
//! selection and register allocation and nothing else; and the final link is krusty's, because
//! zero-toolchain cross-compilation needs it to be. The pipeline is therefore:
//!
//! checked common IR → [`codegen`] (Cranelift) → relocatable object → [`linker`] (with the runtime
//! prebuilt into krusty by `build.rs`) → static ELF executable.
//!
//! The runtime a program links against — allocator, conservative-root/precise-heap collector,
//! object model, strings, `_start`, syscalls — is freestanding C under `runtime/`, compiled once
//! when krusty is built, for every target, and carried inside the compiler ([`prebuilt`]). A user's
//! build touches no C toolchain; building krusty needs a C cross-compiler (clang) or gets no native
//! targets, and says so.
//!
//! Every construct the code generator has not been taught declines with a diagnostic naming it.

mod classes;
mod codegen;
pub mod gc;
mod intrinsics;
mod linker;
mod prebuilt;
pub mod runtime;
mod target;

pub use codegen::CraneliftBackend;
pub use linker::{can_link, link_program, ProgramLinkError};
pub use prebuilt::{prebuilt_available, runtime_objects};
pub use target::{Arch, NativeTarget, Os};
