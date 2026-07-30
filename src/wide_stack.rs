//! A big-stack trampoline for the compiler's recursive passes.
//!
//! The expression passes (checker `expr_inner`, lowering `expr_inner`) recurse over the AST and
//! bound that recursion with explicit depth guards — but the guards are only survivable if every
//! level's stack frames fit. Unoptimized builds give those giant match functions ~120–150 KiB
//! frames (one stack slot per match-arm local, no slot coloring at opt-level 0), so a few dozen
//! levels of nesting exhaust a default 2 MiB test-thread stack long before any guard fires. Each
//! recursive pass entry runs on a thread sized for its depth guard instead, so the guard — not the
//! calling thread's stack — is what limits expression nesting in every build profile and embedder
//! (tests, CLI, LSP).

/// Sized for the deepest depth guard (500) times the worst unoptimized per-level frame total
/// (~165 KiB), with margin; the memory is virtual until touched.
const WIDE_STACK_BYTES: usize = 128 * 1024 * 1024;

/// Run `f` to completion on a dedicated wide-stack thread and return its result.
///
/// This is a strictly SEQUENTIAL handoff, not concurrency: the compiler state `f` borrows is not
/// `Send` (`Rc` handles and `RefCell` caches in the symbol table / platform), but the borrows move
/// to the spawned thread, the parent blocks inside `scope` until it finishes, and the result moves
/// back through `join` (which synchronizes-with the child). No allocation is ever touched by two
/// threads at once, so the non-atomic refcounts and unguarded caches stay single-threaded.
pub(crate) fn on_wide_stack<T>(f: impl FnOnce() -> T) -> T {
    struct AssertSend<T>(T);
    unsafe impl<T> Send for AssertSend<T> {}
    impl<T> AssertSend<T> {
        // Takes WHOLE `self` so the spawned closure captures the wrapper, not (via the edition-2021
        // disjoint-capture rules) the non-`Send` field inside it.
        fn into_inner(self) -> T {
            self.0
        }
    }

    let call = AssertSend(f);
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("krusty-wide-stack".into())
            .stack_size(WIDE_STACK_BYTES)
            .spawn_scoped(scope, move || AssertSend(call.into_inner()()))
            .expect("failed to spawn wide-stack thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            .into_inner()
    })
}
