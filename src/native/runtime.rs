//! The runtime every native program links against: freestanding C under `runtime/`, compiled once
//! per target when krusty itself is built (`build.rs`) and carried inside the compiler
//! ([`super::prebuilt`]). The sources are exposed here so tests can exercise the runtime from C
//! directly, and so the design is documented next to what it documents.
//!
//! **It is freestanding: it does not use a C library.** That is not minimalism for its own sake —
//! it is what makes `docs/BUILD_AND_NATIVE_PLAN.md`'s cross-compilation requirement achievable. Go's
//! defining build property is that `GOOS=linux GOARCH=arm64 go build` works on any machine with
//! nothing installed, because nothing in a Go binary needs a target C toolchain. A runtime that
//! called `printf` would need a target libc, its headers and a target linker for every architecture
//! — the per-target toolchain problem Go exists to avoid. Talking to the kernel directly removes it:
//! `clang --target=<triple>` compiles any registered architecture with no sysroot at krusty's build
//! time, and krusty's own linker ([`super::linker`]) joins the result with a program's objects, so
//! one host produces binaries for all of them and a user's build touches no C toolchain at all.
//!
//! Only the compiler-provided freestanding headers are used (`stdint.h`, `stddef.h`, `stdbool.h`),
//! which C11 §4 guarantees exist without a hosted implementation.
//!
//! The runtime is three translation units, and the split is deliberate:
//!
//! * [`SYS_HEADER`] (`krusty_sys.h`) is the kernel interface — the syscall shim per architecture
//!   and the page-mapping primitives over it. It is the whole of the target-specific surface, and
//!   it is a header of `static inline` functions so that both C files below reach the kernel the
//!   same way without either exporting the other's plumbing.
//! * [`SOURCE`] (`krusty_rt.c`) is the *values*: the built-in types, boxing, strings, rendering and
//!   `kotlin.io`. It allocates only through the collector.
//! * `krusty_gc.c` (see [`super::gc`]) is the heap: allocator and collector. It knows nothing about
//!   any particular type; every object tells it, through its [`KType`](self) descriptor, which of
//!   its fields are references.
//!
//! **Every heap object starts with a type descriptor, and memory is reclaimed.** Allocation goes
//! through `kt_gc_allocate`, and a mark-sweep collector with conservative roots and precise heap
//! tracing frees what is unreachable (`src/native/gc.rs` states the properties and their cost).
//! A `String`'s text is itself a heap object — a byte array — that the string's type lists as a
//! reference, so the collector keeps text alive exactly as long as a string that uses it; a literal
//! keeps pointing into static storage and owns no heap text at all.
//!
//! **The descriptor is also the class.** `KType` names its superclass and carries the vtable, so
//! `is` walks the `super` chain and a method call is `obj->type->vtable[slot]` — the header stays
//! one word and the collector's contract stays `header->type`. `kotlin.Any` is defined here as the
//! root of every chain, with the three slots every table begins with: `equals` (identity),
//! `hashCode` (from the address, which the collector never changes) and `toString`
//! (`<name>@<hex>`). The built-in value types hang off the same root with value equality, so
//! `kt_equals` and `kt_hash_code` answer for a boxed `Int` as Kotlin does. A failed cast and an
//! abstract method exit loudly: there are no exceptions yet, and a wrong answer must not be quiet.
//!
//! Two limits are deliberate and must not be mistaken for oversights:
//!
//! * **`String` is UTF-8 bytes.** Kotlin's `String.length` counts UTF-16 code units, which is not
//!   the byte count for any non-ASCII text. The runtime therefore exposes no `length` at all rather
//!   than exposing a wrong one.
//! * **Floating-point values cannot be rendered.** Kotlin's `Double.toString` is Java's
//!   shortest-round-trip algorithm — `1.0` prints as `1.0` and `1e20` as `1.0E20` — which `printf`'s
//!   `%g` is not, so the libc version was already producing strings Kotlin never would. There is
//!   consequently no `kt_box_double`, which means a `Double` cannot reach a reference position at
//!   all: the backend declines `println(1.0)` at COMPILE time instead of printing something wrong.
//!   Arithmetic and comparison on floating-point values are unaffected.

/// The kernel interface, shared by the value runtime and the collector.
///
/// One syscall shim per supported architecture. Everything above it is portable C, which is why
/// adding an architecture is a matter of adding a register convention and four numbers rather
/// than porting a runtime.
pub const SYS_HEADER: &str = include_str!("runtime/krusty_sys.h");

/// The header a C program that links against the runtime includes.
pub const HEADER: &str = include_str!("runtime/krusty_rt.h");

/// The value runtime: built-in types, boxing, strings, rendering and `kotlin.io`.
pub const SOURCE: &str = include_str!("runtime/krusty_rt.c");

/// The process entry point, per architecture.
///
/// With no C library there is no `crt1.o` to set up a stack frame and call `main`, so the runtime
/// supplies `_start` itself. It has to be assembly: at `_start` the stack pointer is aligned to 16
/// and points at `argc`, whereas a compiled C function's prologue assumes it was CALLED — off by
/// the width of a return address — and the mismatch shows up later as a misaligned vector spill.
pub const START: &str = include_str!("runtime/krusty_start.c");
