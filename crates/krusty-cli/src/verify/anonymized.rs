//! The issue a user submits from a verify report. Unless they choose to share names, it carries
//! totals, difference kinds, unsupported requests and the shape of krusty's errors, and never a
//! compilation, class, member, path or the identifiers an error quotes.

use std::collections::BTreeMap;

use super::record::{CompilationRecord, Outcome};
use super::summary::{describe_kind, megabytes, percent, seconds, Environment, Summary};

/// Where a report is submitted.
pub const NEW_ISSUE_URL: &str = "https://github.com/qnox/krusty/issues/new";

/// The issue template a verify report fills in (`.github/ISSUE_TEMPLATE/verify-report.md`).
const ISSUE_TEMPLATE: &str = "verify-report.md";

/// GitHub truncates or rejects longer prefilled issue links; a longer body is pasted from the
/// written issue file instead.
const URL_LIMIT: usize = 7500;

/// The issue's title.
pub fn issue_title(summary: &Summary) -> String {
    let kotlin = if summary.kotlin_versions.is_empty() {
        String::new()
    } else {
        format!(", Kotlin {}", summary.kotlin_versions.join(", "))
    };
    format!(
        "Verify report: {}/{} compilations compiled, {} of classes identical{kotlin}",
        summary.compiled,
        summary.compilations,
        percent(summary.identical_classes, summary.classes),
    )
}

/// The issue's Markdown body. `per_compilation` adds a row per compilation.
pub fn issue_body(
    records: &[CompilationRecord],
    environment: &Environment,
    share_names: bool,
    per_compilation: bool,
) -> String {
    let summary = Summary::of(records);
    let mut body = String::new();
    body.push_str("### krusty verify report\n\n");
    let optional = |value: &Option<String>| value.clone().unwrap_or_else(|| "unknown".to_string());
    body.push_str(&format!("- krusty: {}\n", environment.krusty_version));
    body.push_str(&format!(
        "- Kotlin: {}\n",
        if summary.kotlin_versions.is_empty() {
            "unknown".to_string()
        } else {
            summary.kotlin_versions.join(", ")
        }
    ));
    body.push_str(&format!(
        "- Build tool: {}\n",
        optional(&environment.build_tool)
    ));
    body.push_str(&format!("- JDK: {}\n", optional(&environment.jdk)));
    body.push_str(&format!("- Platform: {}\n\n", environment.platform));

    body.push_str("| Measure | Value |\n|---|---|\n");
    body.push_str(&format!("| Compilations | {} |\n", summary.compilations));
    body.push_str(&format!(
        "| Compiled by krusty | {} ({}) |\n",
        summary.compiled,
        percent(summary.compiled, summary.compilations)
    ));
    body.push_str(&format!(
        "| Byte-identical compilations | {} |\n",
        summary.identical_compilations
    ));
    body.push_str(&format!("| Failed in krusty | {} |\n", summary.failed));
    body.push_str(&format!(
        "| Unsupported, left to kotlinc | {} |\n",
        summary.unsupported
    ));
    if summary.no_sources > 0 {
        body.push_str(&format!(
            "| Without Kotlin sources | {} |\n",
            summary.no_sources
        ));
    }
    body.push_str(&format!(
        "| Byte-identical classes | {} / {} ({}) |\n",
        summary.identical_classes,
        summary.classes,
        percent(summary.identical_classes, summary.classes)
    ));
    if summary.krusty_millis > 0 {
        body.push_str(&format!(
            "| kotlinc / krusty time (compiled compilations) | {} / {} |\n",
            seconds(summary.kotlinc_millis),
            seconds(summary.krusty_millis)
        ));
    }
    if let Some(peak) = summary.krusty_peak_rss_bytes {
        body.push_str(&format!("| krusty peak memory | {} |\n", megabytes(peak)));
    }

    let ranked = summary.ranked_differences();
    if !ranked.is_empty() {
        body.push_str(
            "\n#### Differences from kotlinc\n\n| Kind | Files | Meaning |\n|---|---|---|\n",
        );
        for (kind, count) in ranked {
            body.push_str(&format!(
                "| `{kind}` | {count} | {} |\n",
                describe_kind(kind)
            ));
        }
    }

    let mask = |text: &str| {
        if share_names {
            text.to_string()
        } else {
            masked_message(text)
        }
    };
    let unsupported = tally(records.iter().flat_map(|record| {
        match &record.outcome {
            Outcome::Unsupported { reasons } => reasons
                .iter()
                .map(|reason| {
                    if share_names {
                        reason.clone()
                    } else {
                        masked_reason(reason)
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }));
    if !unsupported.is_empty() {
        body.push_str(
            "\n#### Unsupported, left to kotlinc\n\n| Reason | Compilations |\n|---|---|\n",
        );
        for (reason, count) in unsupported {
            body.push_str(&format!("| {} | {count} |\n", table_cell(&reason)));
        }
    }
    let errors = tally(records.iter().flat_map(|record| match &record.outcome {
        Outcome::Failed { errors } => errors.iter().map(|error| mask(error)).collect(),
        _ => Vec::new(),
    }));
    if !errors.is_empty() {
        body.push_str("\n#### krusty errors\n\n| Error | Count |\n|---|---|\n");
        for (error, count) in errors.into_iter().take(25) {
            body.push_str(&format!("| {} | {count} |\n", table_cell(&error)));
        }
    }

    if per_compilation {
        body.push_str(
            "\n#### Compilations\n\n| Compilation | Source set | Outcome | Identical classes | kotlinc | krusty | krusty peak |\n|---|---|---|---|---|---|---|\n",
        );
        for (index, record) in records.iter().enumerate() {
            let name = if share_names {
                record.compilation.clone()
            } else {
                format!("compilation {}", index + 1)
            };
            let source_set = match record.source_set.as_deref() {
                Some(set @ ("main" | "test")) => set.to_string(),
                Some(set) if share_names => set.to_string(),
                Some(_) => "other".to_string(),
                None => String::new(),
            };
            let (outcome, classes) = match &record.outcome {
                Outcome::Unsupported { .. } => ("unsupported", String::new()),
                Outcome::Failed { .. } => ("failed", String::new()),
                Outcome::NoSources => ("no sources", String::new()),
                Outcome::Compiled { comparison } => (
                    if comparison.is_identical() {
                        "identical"
                    } else {
                        "compiled"
                    },
                    format!("{} / {}", comparison.identical_classes, comparison.classes),
                ),
            };
            body.push_str(&format!(
                "| {} | {source_set} | {outcome} | {classes} | {} | {} | {} |\n",
                table_cell(&name),
                record.kotlinc_millis.map(seconds).unwrap_or_default(),
                record.krusty_millis.map(seconds).unwrap_or_default(),
                record
                    .krusty_peak_rss_bytes
                    .map(megabytes)
                    .unwrap_or_default(),
            ));
        }
    }
    body.push_str(if share_names {
        "\n<sub>Written by `krusty verify-report`, with names shared.</sub>\n"
    } else {
        "\n<sub>Written by `krusty verify-report`. Compilation, class and member names, paths and the names in errors are left out.</sub>\n"
    });
    body
}

/// A prefilled new-issue link: the whole body when the link stays short enough, otherwise the
/// body without its per-compilation rows, and otherwise only the template, for the user to paste
/// the written issue file into.
pub fn issue_url(
    records: &[CompilationRecord],
    environment: &Environment,
    share_names: bool,
) -> String {
    let title = issue_title(&Summary::of(records));
    for per_compilation in [true, false] {
        let url = prefilled(
            &title,
            Some(&issue_body(
                records,
                environment,
                share_names,
                per_compilation,
            )),
        );
        if url.len() <= URL_LIMIT {
            return url;
        }
    }
    prefilled(&title, None)
}

fn prefilled(title: &str, body: Option<&str>) -> String {
    let mut url = format!(
        "{NEW_ISSUE_URL}?template={ISSUE_TEMPLATE}&title={}",
        percent_encoded(title)
    );
    if let Some(body) = body {
        url.push_str("&body=");
        url.push_str(&percent_encoded(body));
    }
    url
}

fn percent_encoded(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Distinct texts with how often each occurs, most frequent first.
fn tally(texts: impl Iterator<Item = String>) -> Vec<(String, usize)> {
    let mut counts = BTreeMap::<String, usize>::new();
    for text in texts {
        *counts.entry(text).or_default() += 1;
    }
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    ranked
}

fn table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// An error reduced to its shape: without its location, its quoted text, what follows its first
/// `: ` (where a diagnostic names its subject: `conflicting declarations: Foo`), any other word
/// shaped like an identifier, and its paths. `Foo.kt:3:5: error: Unresolved reference 'bar'.`
/// becomes `Unresolved reference '…'.`.
pub(super) fn masked_message(error: &str) -> String {
    let message = error
        .split_once(": error: ")
        .map(|(_, message)| message)
        .or_else(|| error.strip_prefix("error: "))
        .unwrap_or(error);
    let mut masked = String::with_capacity(message.len());
    let mut open: Option<char> = None;
    for character in message.chars() {
        match open {
            Some(quote) if character == quote => {
                masked.push('…');
                masked.push(character);
                open = None;
            }
            Some(_) => {}
            None if matches!(character, '\'' | '`' | '"') => {
                masked.push(character);
                open = Some(character);
            }
            None => masked.push(character),
        }
    }
    // An unclosed quote was an apostrophe: keep what followed it.
    if open.is_some() {
        masked = message.to_string();
    }
    if let Some(colon) = masked.find(": ") {
        masked.truncate(colon);
        masked.push_str(": …");
    }
    let mut first = true;
    let words = masked
        .split(' ')
        .map(|word| {
            let shown = if plain_word(word, first) { word } else { "…" };
            first = false;
            shown
        })
        .collect::<Vec<_>>()
        .join(" ");
    mask_paths(&words)
}

/// Whether a word of an error is prose rather than a name: lowercase letters, with an apostrophe
/// (`can't`), surrounding punctuation, a capital first letter when it opens the message, or the
/// `…` standing in for masked text.
fn plain_word(word: &str, first: bool) -> bool {
    let core = word.trim_matches(|character: char| {
        matches!(
            character,
            '.' | ',' | ':' | ';' | '(' | ')' | '[' | ']' | '\'' | '`' | '"' | '…' | '!' | '?'
        )
    });
    let mut characters = core.chars();
    match characters.next() {
        None => true,
        Some(initial) => {
            (initial.is_ascii_lowercase() || (first && initial.is_ascii_uppercase()))
                && characters.all(|character| character.is_ascii_lowercase() || character == '\'')
        }
    }
}

/// An unsupported request without its paths or the names it carries: an option value naming a
/// qualified declaration (`annotation=com.example.Open`) becomes `…`.
pub(super) fn masked_reason(reason: &str) -> String {
    let masked = mask_paths(reason);
    let mut result = String::with_capacity(masked.len());
    let mut rest = masked.as_str();
    while let Some(equals) = rest.find('=') {
        result.push_str(&rest[..=equals]);
        rest = &rest[equals + 1..];
        let end = rest
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ',' | '\'' | '"' | '`' | ')' | ']')
            })
            .unwrap_or(rest.len());
        let value = &rest[..end];
        if value.trim_end_matches('.').contains('.') {
            result.push('…');
        } else {
            result.push_str(value);
        }
        rest = &rest[end..];
    }
    result.push_str(rest);
    result
}

/// Each path in `text` replaced by `<path>`: a run of characters containing a separator, delimited
/// by whitespace, `=`, `,`, quotes or brackets.
pub(super) fn mask_paths(text: &str) -> String {
    let delimiter = |character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '=' | ',' | '\'' | '"' | '`' | '(' | ')' | '[' | ']'
            )
    };
    let mut masked = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, masked: &mut String| {
        let trailing = word.len() - word.trim_end_matches(['.', ':', ';']).len();
        let (body, end) = word.split_at(word.len() - trailing);
        if body.contains('/') || body.contains('\\') {
            masked.push_str("<path>");
        } else {
            masked.push_str(body);
        }
        masked.push_str(end);
        word.clear();
    };
    for character in text.chars() {
        if delimiter(character) {
            flush(&mut word, &mut masked);
            masked.push(character);
        } else {
            word.push(character);
        }
    }
    flush(&mut word, &mut masked);
    masked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::outputs::{FileDifference, OutputComparison};

    fn environment() -> Environment {
        Environment {
            krusty_version: "0.0.9".to_string(),
            build_tool: Some("Gradle 9.1.0".to_string()),
            jdk: Some("21".to_string()),
            platform: "linux-x86_64".to_string(),
        }
    }

    fn records() -> Vec<CompilationRecord> {
        vec![
            CompilationRecord {
                compilation: ":secret-app:compileKotlin".to_string(),
                source_set: Some("main".to_string()),
                kotlin_version: Some("2.4.20".to_string()),
                outcome: Outcome::Compiled {
                    comparison: OutputComparison {
                        classes: 4,
                        identical_classes: 3,
                        missing: vec![],
                        extra: vec![],
                        differences: vec![FileDifference {
                            path: "com/secret/Billing.class".to_string(),
                            kind: "line-numbers".to_string(),
                            member: Some("charge ()V".to_string()),
                        }],
                    },
                },
                kotlinc_millis: Some(4000),
                krusty_millis: Some(1000),
                krusty_peak_rss_bytes: Some(200 * 1024 * 1024),
            },
            CompilationRecord {
                compilation: ":secret-app:compileIntegrationKotlin".to_string(),
                source_set: Some("integration".to_string()),
                kotlin_version: Some("2.4.20".to_string()),
                outcome: Outcome::Failed {
                    errors: vec![
                        "/home/me/secret/Billing.kt:3:5: error: Unresolved reference 'chargeCard'."
                            .to_string(),
                    ],
                },
                kotlinc_millis: Some(500),
                krusty_millis: Some(100),
                krusty_peak_rss_bytes: None,
            },
            CompilationRecord {
                compilation: ":secret-ui:compileKotlin".to_string(),
                source_set: Some("main".to_string()),
                kotlin_version: Some("2.4.20".to_string()),
                outcome: Outcome::Unsupported {
                    reasons: vec![
                        "krusty: unsupported compiler plugin '/home/me/.gradle/compose.jar'"
                            .to_string(),
                    ],
                },
                kotlinc_millis: Some(700),
                krusty_millis: None,
                krusty_peak_rss_bytes: None,
            },
        ]
    }

    #[test]
    fn the_anonymized_issue_names_no_compilation_class_member_or_path() {
        let body = issue_body(&records(), &environment(), false, true);
        assert_eq!(
            body,
            "### krusty verify report\n\
             \n\
             - krusty: 0.0.9\n\
             - Kotlin: 2.4.20\n\
             - Build tool: Gradle 9.1.0\n\
             - JDK: 21\n\
             - Platform: linux-x86_64\n\
             \n\
             | Measure | Value |\n\
             |---|---|\n\
             | Compilations | 3 |\n\
             | Compiled by krusty | 1 (33.3%) |\n\
             | Byte-identical compilations | 0 |\n\
             | Failed in krusty | 1 |\n\
             | Unsupported, left to kotlinc | 1 |\n\
             | Byte-identical classes | 3 / 4 (75.0%) |\n\
             | kotlinc / krusty time (compiled compilations) | 4.0 s / 1.0 s |\n\
             | krusty peak memory | 200 MB |\n\
             \n\
             #### Differences from kotlinc\n\
             \n\
             | Kind | Files | Meaning |\n\
             |---|---|---|\n\
             | `line-numbers` | 1 | line numbers |\n\
             \n\
             #### Unsupported, left to kotlinc\n\
             \n\
             | Reason | Compilations |\n\
             |---|---|\n\
             | krusty: unsupported compiler plugin '<path>' | 1 |\n\
             \n\
             #### krusty errors\n\
             \n\
             | Error | Count |\n\
             |---|---|\n\
             | Unresolved reference '…'. | 1 |\n\
             \n\
             #### Compilations\n\
             \n\
             | Compilation | Source set | Outcome | Identical classes | kotlinc | krusty | krusty peak |\n\
             |---|---|---|---|---|---|---|\n\
             | compilation 1 | main | compiled | 3 / 4 | 4.0 s | 1.0 s | 200 MB |\n\
             | compilation 2 | other | failed |  | 0.5 s | 0.1 s |  |\n\
             | compilation 3 | main | unsupported |  | 0.7 s |  |  |\n\
             \n\
             <sub>Written by `krusty verify-report`. Compilation, class and member names, paths and the names in errors are left out.</sub>\n"
        );
    }

    #[test]
    fn shared_names_keep_compilations_and_errors() {
        let body = issue_body(&records(), &environment(), true, true);
        assert!(
            body.contains("| :secret-app:compileKotlin | main | compiled |"),
            "{body}"
        );
        assert!(
            body.contains(
                "| /home/me/secret/Billing.kt:3:5: error: Unresolved reference 'chargeCard'. | 1 |"
            ),
            "{body}"
        );
    }

    #[test]
    fn the_issue_link_prefills_the_template_title_and_body() {
        let url = issue_url(&records(), &environment(), false);
        assert!(url.starts_with(
            "https://github.com/qnox/krusty/issues/new?template=verify-report.md&title=Verify%20report%3A%201%2F3%20compilations%20compiled%2C%2075.0%25%20of%20classes%20identical%2C%20Kotlin%202.4.20&body=%23%23%23%20krusty%20verify%20report"
        ), "{url}");
        assert!(!url.contains("secret"), "{url}");
    }

    #[test]
    fn a_long_report_drops_its_compilation_rows_and_then_its_body() {
        let failing = |count: usize, error: &dyn Fn(usize) -> String| {
            (0..count)
                .map(|index| CompilationRecord {
                    compilation: format!(":m{index}:compileKotlin"),
                    source_set: Some("main".to_string()),
                    kotlin_version: None,
                    outcome: Outcome::Failed {
                        errors: vec![error(index)],
                    },
                    kotlinc_millis: None,
                    krusty_millis: None,
                    krusty_peak_rss_bytes: None,
                })
                .collect::<Vec<_>>()
        };
        // The error table keeps the 25 most frequent errors, so many compilations fit without
        // their rows.
        let many = issue_url(
            &failing(500, &|index| {
                format!("error: failure {}", "x".repeat(index % 40 + 1))
            }),
            &environment(),
            false,
        );
        assert!(many.contains("&body="), "{many}");
        assert!(!many.contains("%23%23%23%23%20Compilations"), "{many}");
        let long = issue_url(
            &failing(30, &|index| {
                format!("error: failure {}", "long ".repeat(100 + index))
            }),
            &environment(),
            false,
        );
        assert_eq!(
            long,
            "https://github.com/qnox/krusty/issues/new?template=verify-report.md&title=Verify%20report%3A%200%2F30%20compilations%20compiled%2C%20n%2Fa%20of%20classes%20identical"
        );
    }

    #[test]
    fn paths_and_quoted_identifiers_are_masked() {
        assert_eq!(
            mask_paths("-Xfoo=/a/b.jar, C:\\x\\y.kt and file:///tmp/z."),
            "-Xfoo=<path>, <path> and <path>."
        );
        assert_eq!(
            masked_message(
                "A.kt:1:2: error: Type mismatch: inferred type is `Foo` but \"Bar\" was expected."
            ),
            "Type mismatch: …"
        );
        assert_eq!(masked_message("error: can't infer it"), "can't infer it");
    }

    #[test]
    fn unquoted_names_in_errors_are_masked() {
        assert_eq!(
            masked_message("A.kt:1:2: error: conflicting declarations: SecretCustomer"),
            "conflicting declarations: …"
        );
        assert_eq!(
            masked_message("error: SecretCustomer cannot extend secretBase from com.acme.Billing"),
            "… cannot extend … from …"
        );
        assert_eq!(
            masked_message("error: internal compiler panic: no slot for SecretCustomer.charge"),
            "internal compiler panic: …"
        );
        assert_eq!(
            masked_message("error: 'Secret' is unresolved in Billing_2"),
            "'…' is unresolved in …"
        );
    }

    #[test]
    fn qualified_names_in_unsupported_requests_are_masked() {
        assert_eq!(
            masked_reason(
                "krusty: plugin option 'all-open:annotation=com.acme.Open' at /a/b.jar is not supported"
            ),
            "krusty: plugin option 'all-open:annotation=…' at <path> is not supported"
        );
        assert_eq!(
            masked_reason("krusty does not implement the kotlinc argument '-Xjdk-release=8'."),
            "krusty does not implement the kotlinc argument '-Xjdk-release=8'."
        );
    }
}
