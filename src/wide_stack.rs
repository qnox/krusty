//! Same-thread stack growth for the compiler's depth-bounded recursive passes.
//!
//! The expression passes recurse over parser state or the AST and bound that recursion explicitly,
//! but the bound is only meaningful if the stack survives long enough to reach it. Even after the
//! largest expression dispatchers were split into per-variant helpers, some unoptimized recursive
//! frames remain too large for the whole bound to fit in either a default 2 MiB thread stack or one
//! fixed-size grown segment. Each pass therefore enters on a grown same-thread segment and its
//! recursive expression funnel checks the remaining stack again at every level. `stacker` chains a
//! new segment only when the active one crosses the low-water mark; shallow levels pay only the
//! stack-pointer check. The explicit depth guard — not an embedder's incidental thread-stack size
//! or one segment's capacity — remains the limit in tests, the CLI, and the LSP.

/// Shared semantic expression-nesting contract for the checker and IR lowering.
///
/// The parser counts `parse_bp` entries rather than AST nesting and may spend two entries on one
/// semantic level, so it derives its compatible parse bound from this value instead of copying a
/// second magic number. Keeping the policy here beside the stack-growth mechanism prevents the
/// recursive passes from silently drifting to mutually incompatible acceptance limits.
pub(crate) const MAX_SEMANTIC_EXPR_DEPTH: u32 = 500;

/// The largest measured unoptimized recursive frame is approximately 21 KiB (`ir_lower`'s
/// `expr_inner`, which grew as contract and inline-facade handling moved into the dispatcher),
/// and a `&&`/`||` chain level stacks `expr` + `expr_inner` + the binary-op helper, over 32 KiB
/// per level in an unoptimized build. The 500-level guard can therefore consume more than 16 MiB.
/// Treat 24 MiB as a LOW-WATER MARK and grow by 32 MiB: pass-entry calls establish the first roomy
/// segment, while per-recursion-level calls chain another segment before a large path exhausts the
/// current one. A 16 MiB segment was measured to SIGBUS ~15 levels short of the guard on the
/// 700-operand regression chain; a single 32 MiB segment was later shown insufficient for genuine
/// nested-call paths with larger checker/lowering helper frames.
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
