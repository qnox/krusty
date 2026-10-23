//! Verify mode: a build compiles each compilation with kotlinc as usual and ships kotlinc's
//! output, and krusty compiles the same request beside it. `krusty verify-compile` records one such
//! compilation (whether krusty supports it, compiles it, how long it took and how much memory it
//! needed, and how its output differs from kotlinc's); `krusty verify-report` turns the records of
//! a build into a report the user reads and, anonymized, can submit as a GitHub issue.

mod anonymized;
mod html;
mod layout;
mod markdown;
mod outputs;
mod peak_memory;
mod record;
mod summary;

pub use anonymized::{issue_body, issue_url, NEW_ISSUE_URL};
pub use html::render_html;
pub use layout::check_layout;
pub use markdown::render_markdown;
pub use outputs::{compare_outputs, FileDifference, OutputComparison, ReferenceOutputs};
pub use peak_memory::peak_rss_bytes;
pub use record::{
    CompilationRecord, Outcome, VerifyCompileArguments, VerifyReportArguments,
    VERIFY_COMPILE_USAGE, VERIFY_REPORT_USAGE,
};
pub use summary::{record_line, summary_line, Environment, Summary};

/// What `krusty verify-report` writes into its `--out` directory: the page, the Markdown report and
/// the anonymized issue text.
pub const REPORT_FILES: [&str; 3] = [
    "krusty-verify.html",
    "krusty-verify.md",
    "krusty-verify-issue.md",
];
