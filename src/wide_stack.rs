//! Same-thread stack growth for the compiler's depth-bounded recursive passes.
//!
//! The expression passes (checker `expr_inner`, lowering `expr_inner`) recurse over the AST and
//! bound that recursion with explicit depth guards — but the guards are only survivable if every
//! level's stack frames fit. Even after the largest expression dispatchers were split into
//! per-variant helpers, some unoptimized recursive frames remain large enough that the 500-level
//! bound does not fit on a default 2 MiB test thread. Each recursive pass entry therefore checks
//! the caller's remaining stack and, only when necessary, runs on a temporary stack segment on the
//! SAME thread. The depth guard — not an embedder's incidental thread-stack size — remains the
//! limit in tests, the CLI, and the LSP.

/// The largest remaining measured unoptimized recursive frame is approximately 14 KiB. Its
/// 500-level guard consumes about 7 MiB, so require 8 MiB at pass entry and use a 16 MiB temporary
/// segment when the caller does not have that reserve. A 16 MiB segment also keeps the existing
/// 450-level full-pipeline regression meaningful: its explicitly provisioned 16 MiB test thread
/// already has enough space and does not need stack growth.
const MIN_REMAINING_STACK_BYTES: usize = 8 * 1024 * 1024;
const GROWN_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Run `f` with enough stack for the compiler's recursion bounds and return its result.
///
/// Stack growth stays on the caller's OS thread. That property is required because compiler state
/// deliberately contains non-`Send` `Rc`/`RefCell` values, public runtime traits can be implemented
/// by callers with thread-affine state, and diagnostics/tracing may acquire thread-local context.
/// `stacker::maybe_grow` switches stack segments without weakening any of those type guarantees.
pub(crate) fn on_wide_stack<T>(f: impl FnOnce() -> T) -> T {
    stacker::maybe_grow(MIN_REMAINING_STACK_BYTES, GROWN_STACK_BYTES, f)
}
