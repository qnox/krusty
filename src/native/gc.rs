//! The collector krusty owns.
//!
//! `docs/BUILD_AND_NATIVE_PLAN.md` settles that krusty owns its runtime rather than borrowing
//! another language's, and this is the part that decision turns on. Two properties shape every
//! choice below.
//!
//! **Roots are found conservatively; the heap is traced precisely.** A precise collector must know
//! which stack slots and registers hold references at the moment it runs, which requires stack maps,
//! which requires control of frame layout — and emitting C gives that control to the C compiler. So
//! the stack and the callee-saved registers are scanned *conservatively*: any word that looks like a
//! pointer into the heap keeps its object alive. Inside the heap there is no such limitation, because
//! every object carries its `KType` and the type names exactly which fields are references. That
//! "conservative roots, precise heap" split is the most precision obtainable without a code
//! generator, and it is strictly better than scanning objects conservatively too: a `Long` field
//! holding `0x7f…` cannot retain garbage.
//!
//! **Nothing here needs the compiler's cooperation.** No stack maps, no safepoints, no write
//! barriers. That is what lets the runtime be built and tested now, while the emitter is still C,
//! and it is why this is not the multi-year item. When krusty owns its code generator the tracing,
//! the allocator and the object model survive unchanged; only root-finding is replaced, and it gets
//! more precise rather than differently shaped.
//!
//! The cost, stated so it is chosen rather than discovered: conservative roots forbid *moving*
//! objects, because a word that merely looks like a pointer cannot be updated — it might be an
//! integer. No compaction, no copying nursery, and a stack word that happens to look like a pointer
//! retains whatever it points at.
//!
//! The shape of the heap follows from "never move": objects are segregated by size into chunks,
//! so a candidate address resolves to its object by arithmetic (find the chunk, divide by the
//! chunk's object size) and a freed slot is reused in place by the next allocation of that size.
//! Collection is triggered by allocation VOLUME — bytes handed out since the last collection — not
//! by heap size, so a program with a small live set and a large stream of garbage collects and
//! reuses rather than growing; the threshold rises with the surviving heap so a large live set does
//! not collect constantly.
//!
//! `tests/native_gc_e2e.rs` runs the collector from C and pins each of these properties.

/// Heap, allocator and collector. A separate translation unit from the value runtime so the two
/// can be read independently; they share only what `krusty_rt.h` and `krusty_sys.h` declare.
pub const SOURCE: &str = include_str!("runtime/krusty_gc.c");
