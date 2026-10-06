//! The box byte-equality report as it travels from per-shard reports through `conformance-run.sh`
//! to the badge payload (`conformance-badge.sh`), the `just conformance-badge` preview, and the
//! master-only release publisher.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "krusty-conformance-score-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

/// Run `conformance-run.sh` over `shards` shards of a fake conformance binary that writes
/// `reports[shard]` verbatim as its per-shard report (no report at all for `None`).
#[cfg(unix)]
fn run_scored_shards(name: &str, reports: &[Option<&str>]) -> std::process::Output {
    let temp = scratch_dir(&format!("shards-{name}"));
    for (shard, report) in reports.iter().enumerate() {
        if let Some(report) = report {
            fs::write(temp.join(format!("report-{shard}")), report)
                .expect("write fixture shard report");
        }
    }
    let binary = temp.join("conformance-bin");
    fs::write(
        &binary,
        "#!/usr/bin/env bash\nfixture=\"$(dirname \"$0\")/report-$KRUSTY_CONFORMANCE_SHARD_INDEX\"\n[ ! -e \"$fixture\" ] || cp \"$fixture\" \"$KRUSTY_CONFORMANCE_REPORT\"\n",
    )
    .expect("write reporting conformance fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make reporting conformance fixture executable");
    let output = Command::new("bash")
        .arg(repo_root().join("scripts").join("conformance-run.sh"))
        .arg(&binary)
        .arg("2.4.20")
        .env("KRUSTY_KOTLINC", "/bin/reference-kotlinc")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env(
            "KRUSTY_SCORED_CONFORMANCE_SHARDS",
            reports.len().to_string(),
        )
        .env("KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS", "10")
        .output()
        .expect("run scored conformance fixture");
    fs::remove_dir_all(temp).expect("remove scratch directory");
    output
}

/// Split the runner's stderr into its phase-timing lines, with each whole-second duration replaced
/// by `Ns`, and every other diagnostic line.
#[cfg(unix)]
fn split_phase_timing(stderr: &[u8]) -> (Vec<String>, String) {
    let stderr = String::from_utf8(stderr.to_vec()).expect("runner stderr is UTF-8");
    let mut phases = Vec::new();
    let mut diagnostics = String::new();
    for line in stderr.lines() {
        if let Some(phase) = line.strip_prefix("conformance-run: phase") {
            let normalized = match phase.rsplit_once(' ') {
                Some((head, seconds))
                    if seconds.strip_suffix('s').is_some_and(|n| {
                        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
                    }) =>
                {
                    format!("conformance-run: phase{head} Ns")
                }
                _ => line.to_owned(),
            };
            phases.push(normalized);
        } else {
            diagnostics.push_str(line);
            diagnostics.push('\n');
        }
    }
    (phases, diagnostics)
}

/// The phase-timing lines for a run whose first `ran` of `count` shards started.
#[cfg(unix)]
fn expected_phase_timing(ran: usize, count: usize) -> Vec<String> {
    let labels = (1..=ran)
        .map(|shard| format!("box-shard-{shard}-of-{count}"))
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for label in &labels {
        lines.push(format!("conformance-run: phase start {label}"));
        lines.push(format!("conformance-run: phase {label} Ns"));
    }
    lines.push("conformance-run: phases".to_owned());
    for label in &labels {
        lines.push(format!("conformance-run: phase {label} Ns"));
    }
    lines.push("conformance-run: phase total Ns".to_owned());
    lines
}

fn badge(mode: &str, report: &Path, version: &str) -> std::process::Output {
    Command::new("bash")
        .arg(repo_root().join("scripts").join("conformance-badge.sh"))
        .arg(mode)
        .arg(report)
        .arg(version)
        .output()
        .expect("run conformance badge script")
}

/// Render `report` (written to a scratch `pct.txt`) for Kotlin 2.4.20; returns the output and the
/// report path the script was given.
fn badge_report(name: &str, mode: &str, report: &str) -> (std::process::Output, PathBuf) {
    let temp = scratch_dir(&format!("badge-{name}"));
    let path = temp.join("pct.txt");
    fs::write(&path, report).expect("write badge report");
    let output = badge(mode, &path, "2.4.20");
    fs::remove_dir_all(temp).expect("remove scratch directory");
    (output, path)
}

fn badge_for(name: &str, mode: &str, report: &str) -> std::process::Output {
    badge_report(name, mode, report).0
}

#[cfg(unix)]
#[test]
fn scored_shards_sum_byte_counts_before_deriving_the_percentage() {
    // 1/2 bytes (50%) and 2/8 bytes (25%) weigh 3/10 = 30.0%, not the 37.5% shard average.
    let output = run_scored_shards("weighted", &[Some("50.0 1 2\n"), Some("25.0 2 8\n")]);
    let (phases, diagnostics) = split_phase_timing(&output.stderr);
    assert_eq!(diagnostics, "", "weighted shards wrote diagnostics");
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "30.0 3 10\n");
}

#[cfg(unix)]
#[test]
fn scored_shards_with_no_bytes_report_zero_percent() {
    let output = run_scored_shards("zero", &[Some("0.0 0 0\n"), Some("0.0 0 0\n")]);
    let (phases, diagnostics) = split_phase_timing(&output.stderr);
    assert_eq!(diagnostics, "");
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "0.0 0 0\n");
}

#[cfg(unix)]
#[test]
fn scored_shards_report_real_scale_byte_totals_exactly() {
    let output = run_scored_shards(
        "scale",
        &[
            Some("52.5 12405762 23645354\n"),
            Some("52.5 12405762 23645355\n"),
        ],
    );
    let (phases, diagnostics) = split_phase_timing(&output.stderr);
    assert_eq!(diagnostics, "");
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "52.5 24811524 47290709\n"
    );
}

#[cfg(unix)]
#[test]
fn scored_shards_fail_on_a_malformed_byte_report() {
    let invalid = [
        "50.0 3 2\n",
        "50.0 1\n",
        "50.0 1 2 3\n",
        "x 1 2\n",
        "50 1 2\n",
        "50.0 -1 2\n",
        "50.0 1.5 2\n",
        "50.0 01 2\n",
        "50.0 1 2\n50.0 1 2\n",
        "50.0 1 1234567890123456\n",
    ];
    for report in invalid {
        let output = run_scored_shards("invalid", &[Some("50.0 1 2\n"), Some(report)]);
        assert_eq!(output.status.code(), Some(1), "report {report:?}");
        assert_eq!(output.stdout, b"", "report {report:?}");
        let (phases, diagnostics) = split_phase_timing(&output.stderr);
        assert_eq!(
            diagnostics,
            "conformance test wrote an invalid byte report (want \"<pct> <matched> <total>\" with matched <= total): shard 2/2\n",
            "report {report:?}"
        );
        assert_eq!(phases, expected_phase_timing(2, 2), "report {report:?}");
    }
}

#[cfg(unix)]
#[test]
fn scored_shards_fail_when_a_shard_writes_no_report() {
    for report in [None, Some("")] {
        let output = run_scored_shards("missing", &[Some("50.0 1 2\n"), report]);
        assert_eq!(output.status.code(), Some(1), "report {report:?}");
        assert_eq!(output.stdout, b"", "report {report:?}");
        let (phases, diagnostics) = split_phase_timing(&output.stderr);
        assert_eq!(
            diagnostics, "conformance test did not write its report: shard 2/2\n",
            "report {report:?}"
        );
        assert_eq!(phases, expected_phase_timing(2, 2), "report {report:?}");
    }
}

#[test]
fn badge_fields_carry_the_byte_counts_and_percentage() {
    let output = badge_for("fields", "fields", "30.0 3 10\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "pct=30.0\nmatched=3\ntotal=10\nlabel=Kotlin 2.4.20 byte conformance\nmessage=30.0% (3/10 bytes)\ncolor=orange\n"
    );
}

#[test]
fn badge_json_is_the_shields_endpoint_payload() {
    let output = badge_for("json", "json", "52.5 24811524 47290709\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"schemaVersion\":1,\"label\":\"Kotlin 2.4.20 byte conformance\",\"message\":\"52.5% (24811524/47290709 bytes)\",\"color\":\"yellow\"}\n"
    );
}

#[test]
fn badge_color_follows_the_existing_percentage_thresholds() {
    let cases = [
        ("0.0 0 0\n", "red"),
        ("9.9 99 1000\n", "red"),
        ("10.0 1 10\n", "orange"),
        ("49.9 499 1000\n", "orange"),
        ("50.0 1 2\n", "yellow"),
        ("69.9 699 1000\n", "yellow"),
        ("70.0 7 10\n", "brightgreen"),
        ("100.0 4 4\n", "brightgreen"),
    ];
    for (report, color) in cases {
        let output = badge_for("color", "fields", report);
        assert_eq!(output.status.code(), Some(0), "report {report:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.lines().last(),
            Some(format!("color={color}").as_str()),
            "report {report:?}"
        );
    }
}

#[test]
fn badge_rejects_an_invalid_or_inconsistent_report() {
    for report in ["", "30.0 3\n", "30.0 11 10\n", "30.0 3 10 1\n"] {
        let (output, path) = badge_report("invalid", "fields", report);
        assert_eq!(output.status.code(), Some(1), "report {report:?}");
        assert_eq!(output.stdout, b"", "report {report:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "conformance-badge: invalid byte report (want \"<pct> <matched> <total>\" with matched <= total): {}\n",
                path.display()
            ),
            "report {report:?}"
        );
    }

    let (output, path) = badge_report("inconsistent", "json", "31.0 3 10\n");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "conformance-badge: percentage 31.0% does not match 3/10 bytes: {}\n",
            path.display()
        )
    );

    let missing = std::env::temp_dir().join(format!(
        "krusty-conformance-score-absent-{}/pct.txt",
        std::process::id()
    ));
    let output = badge("fields", &missing, "2.4.20");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "conformance-badge: invalid byte report (want \"<pct> <matched> <total>\" with matched <= total): {}\n",
            missing.display()
        )
    );
}

#[test]
fn badge_preview_and_release_consume_the_same_byte_report() {
    let root = repo_root();
    let justfile = fs::read_to_string(root.join("justfile")).expect("read justfile");
    let recipe = justfile
        .split("\nconformance-badge REPORT=\"\":\n")
        .nth(1)
        .expect("conformance-badge recipe takes an optional REPORT")
        .split("\n\n")
        .next()
        .unwrap();
    assert!(
        recipe.contains("just conformance \"$v\" > \"$report\"")
            && recipe.contains(
                "bash scripts/conformance-badge.sh json \"$report\" \"$v\" > docs/badges/conformance.json.tmp"
            ),
        "the local preview renders the max version's byte report: {recipe}"
    );

    let workflow =
        fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci workflow");
    let lane = workflow
        .find("  conformance:\n")
        .expect("conformance matrix");
    let box_run = &workflow[lane..];
    assert!(
        box_run.contains(
            "just conformance-run \"$PWD/conformance-bin\" \"${{ matrix.version }}\" > pct.txt\n          bash scripts/conformance-badge.sh fields pct.txt \"${{ matrix.version }}\"\n"
        ),
        "each lane validates its byte report as badge fields"
    );
    assert!(
        box_run.contains("name: pct-${{ matrix.version }}\n          path: pct.txt\n"),
        "each lane uploads its byte report"
    );

    let release = &workflow[workflow.find("\n  release:\n").expect("release job")..];
    assert!(
        release.contains("    if: github.ref == 'refs/heads/master'\n"),
        "only master publishes"
    );
    assert!(
        release.contains("name: pct-${{ needs.versions.outputs.max }}\n"),
        "the badge reads the max version's report"
    );
    assert!(
        release.contains(
            "run: bash scripts/conformance-badge.sh fields pct.txt \"$MAX_VERSION\" >> \"$GITHUB_OUTPUT\"\n"
        ),
        "the release badge uses the same payload script"
    );
    assert!(
        release.contains(
            "label: ${{ steps.c.outputs.label }}\n          message: ${{ steps.c.outputs.message }}\n          color: ${{ steps.c.outputs.color }}\n"
        ),
        "the gist receives the computed byte payload"
    );
    assert_eq!(
        workflow.matches("Schneegans/dynamic-badges-action").count(),
        release.matches("Schneegans/dynamic-badges-action").count(),
        "no job other than the master release publishes a badge"
    );
    assert!(
        !workflow.contains("outputs.passed") && !workflow.contains("outputs.applicable"),
        "no workflow step carries box pass counts as the badge score"
    );
}
