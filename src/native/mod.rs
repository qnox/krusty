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
//! What is here so far is the TARGET — the architecture and operating system a build names — the
//! prebuilt runtime that target's link draws on, and the LINKER that draws on it. The code
//! generator whose objects it links follows.

mod captures;
pub mod gc;
// The object MODEL and the tables the CODE GENERATOR reads: which runtime symbol answers a Kotlin member, which names
// are built-in types, which members Kotlin gives a special bridge. It lands next, and the allow
// goes with it; until then nothing in the crate calls these, and their own tests are what
// exercises them.
#[allow(dead_code)]
mod classes;
#[allow(dead_code)]
mod intrinsics;
mod linker;
mod prebuilt;
pub mod runtime;
mod target;

pub use linker::{can_link, link_program, ProgramLinkError};
pub use prebuilt::{prebuilt_available, runtime_objects};
pub use target::{Arch, NativeTarget, Os};
