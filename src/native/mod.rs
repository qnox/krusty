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
//! **Transitional.** The retired C emitter (`emit.rs`, the emitter half of `classes.rs`, `link.rs`,
//! `backend.rs`) is still compiled only because the class and collector tests were written against
//! it; each of those tests is a Kotlin program with expected output, and as the code generator
//! learns to run it the test moves over and the corresponding emitter code is deleted. No new work
//! goes into the C path.
//!
//! Every construct the code generator has not been taught declines with a diagnostic naming it.

pub mod backend;
mod classes;
mod codegen;
mod emit;
mod gc;
mod intrinsics;
mod link;
mod linker;
mod prebuilt;
mod runtime;
mod target;

pub use backend::{
    NativeBackend, ENTRY_SOURCE, GC_SOURCE, RUNTIME_HEADER, RUNTIME_SOURCE, START_SOURCE,
    SYS_HEADER,
};
pub use codegen::CraneliftBackend;
pub use link::{c_compiler, can_build, link_executable, LinkError};
pub use linker::{can_link, link_program, ProgramLinkError};
pub use prebuilt::{prebuilt_available, runtime_objects};
pub use target::{Arch, NativeTarget, Os};
