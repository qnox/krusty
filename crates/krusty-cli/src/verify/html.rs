//! The HTML report: one self-contained page with the totals, the difference kinds, every
//! compilation and a button that opens the anonymized issue.

use super::markdown::LISTED_FILES;
use super::record::{CompilationRecord, Outcome};
use super::summary::{describe_kind, megabytes, percent, seconds, Environment, Summary};

const STYLE: &str = r#"
:root { --bg:#fbfbfa; --fg:#1d1d1b; --muted:#6b6b66; --card:#ffffff; --line:#e4e4df;
  --good:#1f7a4d; --warn:#a15c00; --bad:#b3261e; --accent:#3b5bdb; --bar:#3b5bdb33; }
@media (prefers-color-scheme: dark) { :root { --bg:#161615; --fg:#ececea; --muted:#a3a39c;
  --card:#1f1f1d; --line:#33332f; --good:#5cc28d; --warn:#e0a24a; --bad:#f2847b; --accent:#8da2fb; --bar:#8da2fb33; } }
* { box-sizing:border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif; }
main { max-width:1100px; margin:0 auto; padding:24px 16px 64px; }
h1 { font-size:26px; margin:0 0 4px; } h2 { font-size:18px; margin:32px 0 12px; }
.muted { color:var(--muted); }
.cards { display:grid; grid-template-columns:repeat(auto-fit,minmax(150px,1fr)); gap:12px; margin-top:20px; }
.card { background:var(--card); border:1px solid var(--line); border-radius:10px; padding:14px; }
.card .value { font-size:24px; font-weight:600; } .card .label { color:var(--muted); font-size:13px; }
table { width:100%; border-collapse:collapse; background:var(--card); border:1px solid var(--line); border-radius:10px; overflow:hidden; }
th, td { text-align:left; padding:8px 10px; border-bottom:1px solid var(--line); vertical-align:top; }
th { font-size:13px; color:var(--muted); font-weight:600; } tr:last-child td { border-bottom:none; }
td.num { text-align:right; font-variant-numeric:tabular-nums; white-space:nowrap; }
.scroll { overflow-x:auto; }
.bar { position:relative; } .bar span { position:absolute; inset:4px auto 4px 0; background:var(--bar); border-radius:3px; }
.bar b { position:relative; font-weight:400; }
.tag { display:inline-block; padding:1px 8px; border-radius:999px; font-size:12px; border:1px solid currentColor; }
.identical { color:var(--good); } .compiled { color:var(--accent); } .failed { color:var(--bad); } .unsupported { color:var(--warn); }
code { font:13px ui-monospace,SFMono-Regular,Menlo,monospace; overflow-wrap:anywhere; }
details { margin:8px 0; background:var(--card); border:1px solid var(--line); border-radius:10px; padding:8px 12px; }
summary { cursor:pointer; } details ul { margin:8px 0; padding-left:20px; } pre { white-space:pre-wrap; margin:8px 0; }
.submit { background:var(--card); border:1px solid var(--line); border-radius:10px; padding:16px; margin-top:20px; }
a.button { display:inline-block; background:var(--accent); color:var(--bg); padding:8px 14px; border-radius:8px; text-decoration:none; font-weight:600; }
"#;

pub fn render_html(
    records: &[CompilationRecord],
    environment: &Environment,
    issue_url: &str,
) -> String {
    let summary = Summary::of(records);
    let mut page = String::new();
    page.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">");
    page.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    page.push_str("<title>krusty verify report</title><style>");
    page.push_str(STYLE);
    page.push_str("</style></head><body><main>\n<h1>krusty verify report</h1>\n");
    page.push_str(&format!(
        "<p class=\"muted\">krusty {} · Kotlin {} · {} · JDK {} · {}</p>\n",
        escape(&environment.krusty_version),
        escape(&or_unknown(summary.kotlin_versions.join(", "))),
        escape(
            environment
                .build_tool
                .as_deref()
                .unwrap_or("unknown build tool")
        ),
        escape(environment.jdk.as_deref().unwrap_or("unknown")),
        escape(&environment.platform),
    ));

    page.push_str("<div class=\"cards\">");
    let mut card = |value: String, label: &str| {
        page.push_str(&format!(
            "<div class=\"card\"><div class=\"value\">{}</div><div class=\"label\">{}</div></div>",
            escape(&value),
            escape(label)
        ));
    };
    card(
        format!("{} / {}", summary.compiled, summary.compilations),
        "compilations krusty compiled",
    );
    card(
        percent(summary.identical_classes, summary.classes),
        &format!(
            "classes byte-identical ({} of {})",
            summary.identical_classes, summary.classes
        ),
    );
    card(
        summary.identical_compilations.to_string(),
        "byte-identical compilations",
    );
    card(summary.failed.to_string(), "failed in krusty");
    card(
        summary.unsupported.to_string(),
        "unsupported, left to kotlinc",
    );
    if summary.krusty_millis > 0 {
        card(
            format!(
                "{} → {}",
                seconds(summary.kotlinc_millis),
                seconds(summary.krusty_millis)
            ),
            "kotlinc → krusty compile time",
        );
    }
    if let Some(peak) = summary.krusty_peak_rss_bytes {
        card(megabytes(peak), "krusty peak memory");
    }
    page.push_str("</div>\n");

    page.push_str(&format!(
        "<div class=\"submit\"><b>Help prioritize krusty.</b> Submitting this report as an issue \
         tells the maintainers what to fix first. The issue is anonymized: it holds totals, \
         difference kinds and the shape of krusty's errors, without compilation, class or member \
         names. Review it before you submit; <code>krusty-verify-issue.md</code> beside this page \
         is the same text.<p><a class=\"button\" href=\"{}\">Open the issue on GitHub</a></p></div>\n",
        escape(issue_url)
    ));

    let ranked = summary.ranked_differences();
    if !ranked.is_empty() {
        let most = ranked.first().map_or(1, |(_, count)| *count).max(1);
        page.push_str("<h2>Differences from kotlinc</h2>\n<div class=\"scroll\"><table><tr><th>Kind</th><th>Files</th><th>Meaning</th></tr>\n");
        for (kind, count) in ranked {
            page.push_str(&format!(
                "<tr><td><code>{}</code></td><td class=\"num bar\"><span style=\"width:{}%\"></span><b>{count}</b></td><td>{}</td></tr>\n",
                escape(kind),
                count * 100 / most,
                escape(describe_kind(kind)),
            ));
        }
        page.push_str("</table></div>\n");
    }

    page.push_str("<h2>Compilations</h2>\n<div class=\"scroll\"><table><tr><th>Compilation</th><th>Source set</th><th>Outcome</th><th>Identical classes</th><th>kotlinc</th><th>krusty</th><th>krusty peak</th></tr>\n");
    for record in records {
        let (outcome, classes) = outcome_cells(&record.outcome);
        page.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td><span class=\"tag {outcome}\">{outcome}</span></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>\n",
            escape(&record.compilation),
            escape(record.source_set.as_deref().unwrap_or("")),
            escape(&classes),
            record.kotlinc_millis.map(seconds).unwrap_or_default(),
            record.krusty_millis.map(seconds).unwrap_or_default(),
            record.krusty_peak_rss_bytes.map(megabytes).unwrap_or_default(),
        ));
    }
    page.push_str("</table></div>\n");

    let detailed = records
        .iter()
        .filter(|record| !matches!(&record.outcome, Outcome::Compiled { comparison } if comparison.is_identical()))
        .collect::<Vec<_>>();
    if !detailed.is_empty() {
        page.push_str("<h2>Details</h2>\n");
    }
    for record in detailed {
        let (outcome, _) = outcome_cells(&record.outcome);
        page.push_str(&format!(
            "<details><summary><code>{}</code> <span class=\"tag {outcome}\">{outcome}</span></summary>",
            escape(&record.compilation)
        ));
        match &record.outcome {
            Outcome::Compiled { comparison } => {
                let mut items = Vec::new();
                items.extend(
                    comparison
                        .missing
                        .iter()
                        .map(|path| format!("<code>{}</code>: missing", escape(path))),
                );
                items.extend(
                    comparison
                        .extra
                        .iter()
                        .map(|path| format!("<code>{}</code>: extra", escape(path))),
                );
                items.extend(comparison.differences.iter().map(|difference| {
                    let member = difference
                        .member
                        .as_deref()
                        .map(|member| format!(" in <code>{}</code>", escape(member)))
                        .unwrap_or_default();
                    format!(
                        "<code>{}</code>: {}{member}",
                        escape(&difference.path),
                        escape(&difference.kind)
                    )
                }));
                let total = items.len();
                page.push_str("<ul>");
                for item in items.iter().take(LISTED_FILES) {
                    page.push_str(&format!("<li>{item}</li>"));
                }
                if total > LISTED_FILES {
                    page.push_str(&format!("<li>… and {} more</li>", total - LISTED_FILES));
                }
                page.push_str("</ul>");
            }
            Outcome::Failed { errors } => {
                page.push_str(&format!("<pre>{}</pre>", escape(&errors.join("\n"))));
            }
            Outcome::Unsupported { reasons } => {
                page.push_str("<ul>");
                for reason in reasons {
                    page.push_str(&format!("<li>{}</li>", escape(reason)));
                }
                page.push_str("</ul>");
            }
        }
        page.push_str("</details>\n");
    }
    page.push_str("</main></body></html>\n");
    page
}

fn outcome_cells(outcome: &Outcome) -> (&'static str, String) {
    match outcome {
        Outcome::Unsupported { .. } => ("unsupported", String::new()),
        Outcome::Failed { .. } => ("failed", String::new()),
        Outcome::Compiled { comparison } => (
            if comparison.is_identical() {
                "identical"
            } else {
                "compiled"
            },
            format!("{} / {}", comparison.identical_classes, comparison.classes),
        ),
    }
}

fn or_unknown(text: String) -> String {
    if text.is_empty() {
        "unknown".to_string()
    } else {
        text
    }
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_escapes_what_the_build_named() {
        let record = CompilationRecord {
            compilation: ":a<b>:compileKotlin".to_string(),
            source_set: None,
            kotlin_version: None,
            outcome: Outcome::Failed {
                errors: vec!["A.kt:1:1: error: <bad> & 'worse'".to_string()],
            },
            kotlinc_millis: None,
            krusty_millis: None,
            krusty_peak_rss_bytes: None,
        };
        let page = render_html(
            &[record],
            &Environment::default(),
            "https://x.test/?a=1&b=2",
        );
        assert!(page.contains("<code>:a&lt;b&gt;:compileKotlin</code>"));
        assert!(page.contains("<pre>A.kt:1:1: error: &lt;bad&gt; &amp; &#39;worse&#39;</pre>"));
        assert!(page.contains("href=\"https://x.test/?a=1&amp;b=2\""));
        assert!(!page.contains("<bad>"));
    }
}
