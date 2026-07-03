//! Opt-in compiler tracing — **zero cost unless built with `--features trace`**.
//!
//! Diagnostics in compiler code go through [`trace_compiler!`], never `eprintln!`/`println!`/`dbg!`
//! (the differential harness parses stdout/stderr, so stray prints can corrupt it).
//!
//! # Enabling
//! Tracing is gated by the `trace` cargo feature, which is **off by default** — every trace site then
//! compiles to nothing (the args are still type-checked but dead-code-eliminated, so locals used only
//! for tracing don't warn). To diagnose:
//! ```text
//! cargo build --features trace --bin krusty           # or: cargo test --features trace …
//! KRUSTY_TRACE=all ./target/debug/krusty …            # all categories
//! KRUSTY_TRACE=resolve,suspend ./target/debug/krusty … # a comma-separated subset
//! ```
//! With the feature on, a disabled category costs one read of a process-once-cached env decision.
//!
//! # Categories
//! The category is the first macro argument (a string literal). The canonical set — keep new sites to
//! these so `KRUSTY_TRACE=<cat>` stays predictable (see [`CATEGORIES`]):
//! - `resolve` — name/overload/type resolution in the checker.
//! - `lower` — frontend-to-IR lowering decisions and bail reasons.
//! - `suspend` — coroutine / suspend-function lowering (`jvm/suspend.rs`).
//! - `value_classes` — inline/value-class transform (`jvm/value_classes.rs`).
//! - `splice` — inline-function bytecode splicing (`jvm/ir_emit.rs`).

/// The canonical trace categories (see the module docs). Listed here so the set is discoverable in one
/// place; `KRUSTY_TRACE=all` enables every category regardless.
pub const CATEGORIES: &[&str] = &["resolve", "lower", "suspend", "value_classes", "splice"];

#[cfg(feature = "trace")]
mod imp {
    use std::sync::OnceLock;

    static TRACE: OnceLock<Option<Vec<String>>> = OnceLock::new();

    /// The configured category list, parsed once from `KRUSTY_TRACE`. `None` ⇒ tracing off.
    fn configured() -> &'static Option<Vec<String>> {
        TRACE.get_or_init(|| {
            let raw = std::env::var("KRUSTY_TRACE").ok()?;
            let cats: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            (!cats.is_empty()).then_some(cats)
        })
    }

    /// Whether `category` (or `all`) is enabled. Reads the cached decision; no allocation/formatting.
    pub fn enabled(category: &str) -> bool {
        configured()
            .as_ref()
            .is_some_and(|cats| cats.iter().any(|c| c == "all" || c == category))
    }

    /// Emit a formatted trace line on stderr. Only called after [`enabled`] returns true.
    pub fn emit(category: &str, args: std::fmt::Arguments<'_>) {
        eprintln!("[{category}] {args}");
    }
}

#[cfg(feature = "trace")]
pub use imp::{emit, enabled};

/// Emit a diagnostic trace line under `category` when tracing is enabled for it.
///
/// With the `trace` feature **off** (the default) this expands to nothing — the arguments are still
/// type-checked (so locals used only for tracing don't warn) but the `if false` block is
/// dead-code-eliminated, leaving zero runtime cost. With the feature **on**, the line is emitted only
/// when `KRUSTY_TRACE` selects `category` (or `all`).
#[cfg(feature = "trace")]
#[macro_export]
macro_rules! trace_compiler {
    ($category:literal, $($arg:tt)*) => {{
        if $crate::trace::enabled($category) {
            $crate::trace::emit($category, format_args!($($arg)*));
        }
    }};
}

#[cfg(not(feature = "trace"))]
#[macro_export]
macro_rules! trace_compiler {
    ($category:literal, $($arg:tt)*) => {{
        // Tracing compiled out: keep the args "used" (no unused-variable warnings) but emit no code.
        if false {
            let _ = ($category, format_args!($($arg)*));
        }
    }};
}
