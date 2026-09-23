//! Native backend — Kotlin to a self-contained executable, with no JVM in the result and no other
//! compiler in the loop.
//!
//! The decisions this module implements, in order: krusty owns its runtime; krusty owns its code generator, so it emits no C; Cranelift owns instruction
//! selection and register allocation and nothing else; and the final link is krusty's, because
//! zero-toolchain cross-compilation needs it to be. The pipeline is therefore:
//!
//! checked common IR → codegen (Cranelift) → relocatable object → linker (with the runtime
//! prebuilt into krusty by `build.rs`) → static ELF executable.
//!
//! The runtime a program links against — allocator, conservative-root/precise-heap collector,
//! object model, strings, `_start`, syscalls — is freestanding C under `runtime/`, compiled once
//! when krusty is built, for every target, and carried inside the compiler ([`prebuilt`]). A user's
//! build touches no C toolchain; building krusty needs a C cross-compiler (clang) or gets no native
//! targets, and says so.
//!
//! What is here so far is the TARGET — the architecture and operating system a build names — and
//! the prebuilt runtime that target's link will draw on. The linker and the code generator follow.

mod prebuilt;
mod target;

pub use prebuilt::{prebuilt_available, runtime_objects};
pub use target::{Arch, NativeTarget, Os};
