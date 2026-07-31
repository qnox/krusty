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

/// The largest measured unoptimized recursive frame is approximately 21 KiB (`ir_lower`'s
/// `expr_inner`, which grew as contract and inline-facade handling moved into the dispatcher),
/// and a `&&`/`||` chain level stacks `expr` + `expr_inner` + the binary-op helper, over 32 KiB
/// per level in an unoptimized build. The 500-level guard can therefore consume more than 16 MiB
/// before the guard trips, so require 24 MiB at pass entry and grow to a 32 MiB temporary segment
/// when the caller does not have that reserve — 16 MiB was measured to SIGBUS ~15 levels short of
/// the guard on the 700-operand regression chain. The depth guard — not an embedder's incidental
/// thread-stack size — remains the limit in tests, the CLI, and the LSP.
const MIN_REMAINING_STACK_BYTES: usize = 24 * 1024 * 1024;
const GROWN_STACK_BYTES: usize = 32 * 1024 * 1024;

/// Run `f` with enough stack for the compiler's recursion bounds and return its result.
///
/// Stack growth stays on the caller's OS thread. That property is required because compiler state
/// deliberately contains non-`Send` `Rc`/`RefCell` values, public runtime traits can be implemented
/// by callers with thread-affine state, and diagnostics/tracing may acquire thread-local context.
/// `stacker::maybe_grow` switches stack segments without weakening any of those type guarantees.
pub(crate) fn on_wide_stack<T>(f: impl FnOnce() -> T) -> T {
    stacker::maybe_grow(MIN_REMAINING_STACK_BYTES, GROWN_STACK_BYTES, f)
}
