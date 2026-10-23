//! The Markdown report a user reads locally: the issue text with names, then each compilation's
//! differing files.

use super::anonymized::issue_body;
use super::record::{CompilationRecord, Outcome};
use super::summary::Environment;

/// At most this many differing files are listed per compilation.
pub(super) const LISTED_FILES: usize = 200;

pub fn render_markdown(
    records: &[CompilationRecord],
    environment: &Environment,
    issue_url: &str,
) -> String {
    let mut report = issue_body(records, environment, true, true);
    report.push_str("\n### Submit this report\n\n");
    report.push_str(&format!(
        "Help prioritize krusty's work by [opening an issue with the anonymized report]({issue_url}). \
         It holds totals, difference kinds and the shape of krusty's errors, without compilation, \
         class or member names; `krusty-verify-issue.md` beside this file is the same text.\n"
    ));
    for record in records {
        let details = match &record.outcome {
            Outcome::Compiled { comparison } if !comparison.is_identical() => {
                let mut lines = Vec::new();
                lines.extend(
                    comparison
                        .missing
                        .iter()
                        .map(|path| format!("- `{path}`: missing")),
                );
                lines.extend(
                    comparison
                        .extra
                        .iter()
                        .map(|path| format!("- `{path}`: extra")),
                );
                lines.extend(comparison.differences.iter().map(
                    |difference| match &difference.member {
                        Some(member) => {
                            format!("- `{}`: {} in `{member}`", difference.path, difference.kind)
                        }
                        None => format!("- `{}`: {}", difference.path, difference.kind),
                    },
                ));
                let total = lines.len();
                lines.truncate(LISTED_FILES);
                if total > LISTED_FILES {
                    lines.push(format!("- … and {} more", total - LISTED_FILES));
                }
                lines
            }
            Outcome::Failed { errors } => {
                errors.iter().map(|error| format!("    {error}")).collect()
            }
            Outcome::Unsupported { reasons } => {
                reasons.iter().map(|reason| format!("- {reason}")).collect()
            }
            Outcome::Compiled { .. } => continue,
        };
        report.push_str(&format!("\n#### `{}`\n\n", record.compilation));
        for line in details {
            report.push_str(&line);
            report.push('\n');
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::outputs::{FileDifference, OutputComparison};

    #[test]
    fn the_local_report_lists_each_differing_file() {
        let record = CompilationRecord {
            compilation: ":app:compileKotlin".to_string(),
            source_set: Some("main".to_string()),
            kotlin_version: None,
            outcome: Outcome::Compiled {
                comparison: OutputComparison {
                    classes: 2,
                    identical_classes: 0,
                    missing: vec!["a/Gone.class".to_string()],
                    extra: vec![],
                    differences: vec![FileDifference {
                        path: "a/B.class".to_string(),
                        kind: "instructions".to_string(),
                        member: Some("run ()V".to_string()),
                    }],
                },
            },
            kotlinc_millis: None,
            krusty_millis: None,
            krusty_peak_rss_bytes: None,
        };
        let report = render_markdown(
            &[record],
            &Environment::default(),
            "https://example.test/issue",
        );
        assert!(
            report.ends_with(
                "\n#### `:app:compileKotlin`\n\n- `a/Gone.class`: missing\n- `a/B.class`: instructions in `run ()V`\n"
            ),
            "{report}"
        );
        assert!(report
            .contains("[opening an issue with the anonymized report](https://example.test/issue)"));
    }
}
