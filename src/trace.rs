//! Lightweight opt-in compiler tracing.
//!
//! Set `KRUSTY_TRACE=all` or a comma-separated category list such as
//! `KRUSTY_TRACE=resolve,inline` to emit diagnostic trace lines on stderr. Disabled traces avoid
//! formatting and read the environment only once.

use std::sync::OnceLock;

static TRACE: OnceLock<Option<Vec<String>>> = OnceLock::new();

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

pub fn enabled(category: &str) -> bool {
    configured()
        .as_ref()
        .is_some_and(|cats| cats.iter().any(|c| c == "all" || c == category))
}

pub fn emit(category: &str, args: std::fmt::Arguments<'_>) {
    eprintln!("[{category}] {args}");
}

#[macro_export]
macro_rules! trace_compiler {
    ($category:literal, $($arg:tt)*) => {{
        if $crate::trace::enabled($category) {
            $crate::trace::emit($category, format_args!($($arg)*));
        }
    }};
}
