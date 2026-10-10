//! Target box reports as they travel from test processes through their runners to badge payloads,
//! the `just conformance-badge` preview, the master-only release publisher, and the public copy:
//! JVM `<pct> <passed> <applicable>` and `<pct> <matched> <total>` byte reports, plus independent
//! Native `<pct> <passed> <applicable>` counts.

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

/// One shard's fixture reports: the case report and the JVM byte report it writes verbatim (no
/// file at all for `None`).
type ShardReports<'a> = (Option<&'a str>, Option<&'a str>);

/// The runner's output and, when it was asked for one, the JVM byte report file it left behind.
struct ScoredRun {
    output: std::process::Output,
    bytes: Option<String>,
}

/// Run `conformance-run.sh` over one shard per `reports` entry with a fake conformance binary that
/// writes that entry's reports. With `byte_out`, the runner also gets a JVM byte report path.
#[cfg(unix)]
fn run_scored_shards(name: &str, reports: &[ShardReports], byte_out: bool) -> ScoredRun {
    let temp = scratch_dir(&format!("shards-{name}"));
    for (shard, (cases, bytes)) in reports.iter().enumerate() {
        if let Some(cases) = cases {
            fs::write(temp.join(format!("cases-{shard}")), cases)
                .expect("write fixture shard case report");
        }
        if let Some(bytes) = bytes {
            fs::write(temp.join(format!("bytes-{shard}")), bytes)
                .expect("write fixture shard JVM byte report");
        }
    }
    let binary = temp.join("conformance-bin");
    fs::write(
        &binary,
        "#!/usr/bin/env bash\ndir=\"$(dirname \"$0\")\"\n[ ! -e \"$dir/cases-$KRUSTY_CONFORMANCE_SHARD_INDEX\" ] || cp \"$dir/cases-$KRUSTY_CONFORMANCE_SHARD_INDEX\" \"$KRUSTY_CONFORMANCE_REPORT\"\n[ ! -e \"$dir/bytes-$KRUSTY_CONFORMANCE_SHARD_INDEX\" ] || cp \"$dir/bytes-$KRUSTY_CONFORMANCE_SHARD_INDEX\" \"$KRUSTY_JVM_BYTE_REPORT\"\n",
    )
    .expect("write reporting conformance fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make reporting conformance fixture executable");
    let byte_report = temp.join("jvm-byte-equality.txt");
    // A stale report from an earlier run must not survive a run that stops before its totals.
    fs::write(&byte_report, "99.9 999 1000\n").expect("write stale JVM byte report");
    let mut command = Command::new("bash");
    command
        .arg(repo_root().join("scripts").join("conformance-run.sh"))
        .arg(&binary)
        .arg("2.4.20");
    if byte_out {
        command.arg(&byte_report);
    }
    let output = command
        .env("KRUSTY_KOTLINC", "/bin/reference-kotlinc")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env(
            "KRUSTY_SCORED_CONFORMANCE_SHARDS",
            reports.len().to_string(),
        )
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "10")
        .output()
        .expect("run scored conformance fixture");
    let bytes = byte_out
        .then(|| fs::read_to_string(&byte_report).expect("read the runner's JVM byte report"));
    fs::remove_dir_all(temp).expect("remove scratch directory");
    ScoredRun { output, bytes }
}

/// The runner's stderr summary of the combined JVM byte report `line`.
fn byte_summary(line: &str) -> String {
    format!(
        "conformance-run: Kotlin 2.4.20 JVM byte equality (matched/total .class bytes): {line}\n"
    )
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

fn badge(mode: &str, kind: &str, report: &Path, version: &str) -> std::process::Output {
    Command::new("bash")
        .arg(repo_root().join("scripts").join("conformance-badge.sh"))
        .arg(mode)
        .arg(kind)
        .arg(report)
        .arg(version)
        .output()
        .expect("run conformance badge script")
}

/// Render `report` (written to a scratch file) as a `kind` badge for Kotlin 2.4.20; returns the
/// output and the report path the script was given.
fn badge_report(
    name: &str,
    mode: &str,
    kind: &str,
    report: &str,
) -> (std::process::Output, PathBuf) {
    let temp = scratch_dir(&format!("badge-{name}"));
    let path = temp.join("report.txt");
    fs::write(&path, report).expect("write badge report");
    let output = badge(mode, kind, &path, "2.4.20");
    fs::remove_dir_all(temp).expect("remove scratch directory");
    (output, path)
}

fn badge_for(name: &str, mode: &str, kind: &str, report: &str) -> std::process::Output {
    badge_report(name, mode, kind, report).0
}

const CASE_SHAPE: &str = "\"<pct> <passed> <applicable>\" with passed <= applicable";
const BYTE_SHAPE: &str = "\"<pct> <matched> <total>\" with matched <= total";

#[cfg(unix)]
#[test]
fn scored_shards_sum_each_reports_counts_before_deriving_its_percentage() {
    // Bytes 1/2 (50%) and 2/8 (25%) weigh 3/10 = 30.0%, not the 37.5% shard average; the case
    // counts 1/1 and 1/2 weigh 2/3 = 66.6% rounded down, not 75%, independently of the bytes.
    let run = run_scored_shards(
        "weighted",
        &[
            (Some("100.0 1 1\n"), Some("50.0 1 2\n")),
            (Some("50.0 1 2\n"), Some("25.0 2 8\n")),
        ],
        true,
    );
    let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
    assert_eq!(diagnostics, byte_summary("30.0 3 10"));
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(run.output.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.output.stdout).unwrap(), "66.6 2 3\n");
    assert_eq!(run.bytes.as_deref(), Some("30.0 3 10\n"));
}

#[cfg(unix)]
#[test]
fn scored_shards_without_a_byte_report_path_still_validate_and_print_both() {
    let run = run_scored_shards(
        "no-byte-path",
        &[
            (Some("100.0 1 1\n"), Some("50.0 1 2\n")),
            (Some("50.0 1 2\n"), Some("25.0 2 8\n")),
        ],
        false,
    );
    let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
    assert_eq!(diagnostics, byte_summary("30.0 3 10"));
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(run.output.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.output.stdout).unwrap(), "66.6 2 3\n");
}

#[cfg(unix)]
#[test]
fn scored_shards_with_no_cases_or_bytes_report_zero_percent() {
    let run = run_scored_shards(
        "zero",
        &[
            (Some("0.0 0 0\n"), Some("0.0 0 0\n")),
            (Some("0.0 0 0\n"), Some("0.0 0 0\n")),
        ],
        true,
    );
    let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
    assert_eq!(diagnostics, byte_summary("0.0 0 0"));
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(run.output.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.output.stdout).unwrap(), "0.0 0 0\n");
    assert_eq!(run.bytes.as_deref(), Some("0.0 0 0\n"));
}

#[cfg(unix)]
#[test]
fn scored_shards_report_real_scale_totals_exactly() {
    let run = run_scored_shards(
        "scale",
        &[
            (Some("88.4 3154 3567\n"), Some("52.4 12405762 23645354\n")),
            (Some("88.3 3153 3568\n"), Some("52.4 12405762 23645355\n")),
        ],
        true,
    );
    let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
    assert_eq!(diagnostics, byte_summary("52.4 24811524 47290709"));
    assert_eq!(phases, expected_phase_timing(2, 2));
    assert_eq!(run.output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(run.output.stdout).unwrap(),
        "88.3 6307 7135\n"
    );
    assert_eq!(run.bytes.as_deref(), Some("52.4 24811524 47290709\n"));
}

const MALFORMED_REPORTS: [&str; 10] = [
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

#[cfg(unix)]
#[test]
fn scored_shards_fail_on_a_malformed_case_report() {
    for report in MALFORMED_REPORTS {
        let run = run_scored_shards(
            "invalid-cases",
            &[
                (Some("50.0 1 2\n"), Some("50.0 1 2\n")),
                (Some(report), Some("50.0 1 2\n")),
            ],
            true,
        );
        assert_eq!(run.output.status.code(), Some(1), "report {report:?}");
        assert_eq!(run.output.stdout, b"", "report {report:?}");
        assert_eq!(run.bytes.as_deref(), Some(""), "report {report:?}");
        let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
        assert_eq!(
            diagnostics,
            format!(
                "conformance test wrote an invalid case report (want {CASE_SHAPE}): shard 2/2\n"
            ),
            "report {report:?}"
        );
        assert_eq!(phases, expected_phase_timing(2, 2), "report {report:?}");
    }
}

#[cfg(unix)]
#[test]
fn scored_shards_fail_on_a_malformed_byte_report() {
    for report in MALFORMED_REPORTS {
        let run = run_scored_shards(
            "invalid-bytes",
            &[
                (Some("50.0 1 2\n"), Some("50.0 1 2\n")),
                (Some("50.0 1 2\n"), Some(report)),
            ],
            true,
        );
        assert_eq!(run.output.status.code(), Some(1), "report {report:?}");
        assert_eq!(run.output.stdout, b"", "report {report:?}");
        assert_eq!(run.bytes.as_deref(), Some(""), "report {report:?}");
        let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
        assert_eq!(
            diagnostics,
            format!(
                "conformance test wrote an invalid JVM byte report (want {BYTE_SHAPE}): shard 2/2\n"
            ),
            "report {report:?}"
        );
        assert_eq!(phases, expected_phase_timing(2, 2), "report {report:?}");
    }
}

#[cfg(unix)]
#[test]
fn scored_shards_fail_when_a_shard_writes_either_report_empty_or_not_at_all() {
    for report in [None, Some("")] {
        for (shard, kind) in [
            ((report, Some("50.0 1 2\n")), "case report"),
            ((Some("50.0 1 2\n"), report), "JVM byte report"),
        ] {
            let run = run_scored_shards(
                "missing",
                &[(Some("50.0 1 2\n"), Some("50.0 1 2\n")), shard],
                true,
            );
            assert_eq!(run.output.status.code(), Some(1), "{kind} {report:?}");
            assert_eq!(run.output.stdout, b"", "{kind} {report:?}");
            assert_eq!(run.bytes.as_deref(), Some(""), "{kind} {report:?}");
            let (phases, diagnostics) = split_phase_timing(&run.output.stderr);
            assert_eq!(
                diagnostics,
                format!("conformance test did not write its {kind}: shard 2/2\n"),
                "{kind} {report:?}"
            );
            assert_eq!(phases, expected_phase_timing(2, 2), "{kind} {report:?}");
        }
    }
}

#[test]
fn conformance_badge_keeps_the_original_case_payload() {
    let output = badge_for("case-fields", "fields", "conformance", "66.6 2 3\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "pct=66.6\npassed=2\napplicable=3\nlabel=Kotlin 2.4.20 conformance\nmessage=66.6% (2/3)\ncolor=yellow\n"
    );

    let output = badge_for("case-json", "json", "conformance", "88.3 6307 7135\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"schemaVersion\":1,\"label\":\"Kotlin 2.4.20 conformance\",\"message\":\"88.3% (6307/7135)\",\"color\":\"brightgreen\"}\n"
    );
}

#[test]
fn jvm_byte_equality_badge_carries_the_byte_counts() {
    let output = badge_for("bytes-fields", "fields", "jvm-byte-equality", "30.0 3 10\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "pct=30.0\nmatched=3\ntotal=10\nlabel=Kotlin 2.4.20 JVM byte equality\nmessage=30.0% (3/10 bytes)\ncolor=orange\n"
    );

    let output = badge_for(
        "bytes-json",
        "json",
        "jvm-byte-equality",
        "52.4 24811524 47290709\n",
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"schemaVersion\":1,\"label\":\"Kotlin 2.4.20 JVM byte equality\",\"message\":\"52.4% (24811524/47290709 bytes)\",\"color\":\"yellow\"}\n"
    );

    let output = badge_for("bytes-zero", "json", "jvm-byte-equality", "0.0 0 0\n");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"schemaVersion\":1,\"label\":\"Kotlin 2.4.20 JVM byte equality\",\"message\":\"0.0% (0/0 bytes)\",\"color\":\"red\"}\n"
    );
}

#[test]
fn native_conformance_badge_carries_its_own_case_counts() {
    let output = badge_for(
        "native-fields",
        "fields",
        "native-conformance",
        "12.5 1 8\n",
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "pct=12.5\npassed=1\napplicable=8\nlabel=Kotlin 2.4.20 Native conformance\nmessage=12.5% (1/8)\ncolor=orange\n"
    );
}

#[test]
fn all_conformance_badges_follow_the_existing_percentage_thresholds() {
    let cases = [
        ("0.0 0 0\n", "red"),
        ("9.9 99 1000\n", "red"),
        ("10.0 1 10\n", "orange"),
        ("49.9 499 1000\n", "orange"),
        ("50.0 1 2\n", "yellow"),
        ("69.9 699 1000\n", "yellow"),
        ("70.0 7 10\n", "brightgreen"),
        ("99.9 3145 3146\n", "brightgreen"),
        ("100.0 4 4\n", "brightgreen"),
    ];
    for kind in ["conformance", "jvm-byte-equality", "native-conformance"] {
        for (report, color) in cases {
            let output = badge_for("color", "fields", kind, report);
            assert_eq!(output.status.code(), Some(0), "{kind} report {report:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                stdout.lines().last(),
                Some(format!("color={color}").as_str()),
                "{kind} report {report:?}"
            );
        }
    }
}

#[test]
fn badges_reject_an_invalid_or_inconsistent_report() {
    for (kind, name, shape, noun) in [
        ("conformance", "case report", CASE_SHAPE, "cases"),
        ("jvm-byte-equality", "JVM byte report", BYTE_SHAPE, "bytes"),
        (
            "native-conformance",
            "Native case report",
            CASE_SHAPE,
            "cases",
        ),
    ] {
        for report in ["", "30.0 3\n", "30.0 11 10\n", "30.0 3 10 1\n"] {
            let (output, path) = badge_report("invalid", "fields", kind, report);
            assert_eq!(output.status.code(), Some(1), "{kind} report {report:?}");
            assert_eq!(output.stdout, b"", "{kind} report {report:?}");
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!(
                    "conformance-badge: invalid {name} (want {shape}): {}\n",
                    path.display()
                ),
                "{kind} report {report:?}"
            );
        }

        let (output, path) = badge_report("inconsistent", "json", kind, "31.0 3 10\n");
        assert_eq!(output.status.code(), Some(1), "{kind}");
        assert_eq!(output.stdout, b"", "{kind}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "conformance-badge: percentage 31.0% does not match 3/10 {noun}: {}\n",
                path.display()
            ),
            "{kind}"
        );

        let missing = std::env::temp_dir().join(format!(
            "krusty-conformance-score-absent-{}/report.txt",
            std::process::id()
        ));
        let output = badge("fields", kind, &missing, "2.4.20");
        assert_eq!(output.status.code(), Some(1), "{kind}");
        assert_eq!(output.stdout, b"", "{kind}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "conformance-badge: invalid {name} (want {shape}): {}\n",
                missing.display()
            ),
            "{kind}"
        );
    }
}

#[test]
fn badge_rejects_an_unknown_kind_or_mode() {
    for (mode, kind) in [("fields", "bytes"), ("json", ""), ("svg", "conformance")] {
        let (output, _) = badge_report("usage", mode, kind, "30.0 3 10\n");
        assert_eq!(output.status.code(), Some(2), "{mode} {kind}");
        assert_eq!(output.stdout, b"", "{mode} {kind}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.starts_with("# Turn one target report into its shields.io badge"),
            "{mode} {kind}: {stderr}"
        );
    }
}

/// The text of the `job` job in the CI workflow, up to the next two-space-indented job key.
fn workflow_job<'a>(workflow: &'a str, job: &str) -> &'a str {
    let start = workflow
        .find(&format!("\n  {job}:\n"))
        .unwrap_or_else(|| panic!("workflow has a {job} job"))
        + 1;
    let rest = &workflow[start..];
    let mut offset = 0;
    for (index, line) in rest.split_inclusive('\n').enumerate() {
        let next_job = line
            .strip_prefix("  ")
            .is_some_and(|key| key.starts_with(|c: char| c.is_ascii_alphanumeric()));
        if index > 0 && next_job {
            return &rest[..offset];
        }
        offset += line.len();
    }
    rest
}

#[test]
fn preview_lanes_and_release_carry_all_target_reports_distinctly() {
    let root = repo_root();
    let justfile = fs::read_to_string(root.join("justfile")).expect("read justfile");
    let recipe = justfile
        .split("\nconformance-badge CASES=\"\" BYTES=\"\" NATIVE=\"\":\n")
        .nth(1)
        .expect("conformance-badge recipe takes optional CASES and BYTES")
        .split("\n\n")
        .next()
        .unwrap();
    for needle in [
        "just conformance \"$v\" \"$bytes\" > \"$cases\"",
        "just native-conformance-run \"$(just conformance-bin)\" \"$v\" \"$native\"",
        "bash scripts/conformance-badge.sh json conformance \"$cases\" \"$v\" > docs/badges/conformance.json.tmp",
        "bash scripts/conformance-badge.sh json jvm-byte-equality \"$bytes\" \"$v\" \\\n      > docs/badges/jvm-byte-equality.json.tmp",
        "bash scripts/conformance-badge.sh json native-conformance \"$native\" \"$v\" \\\n      > docs/badges/native-conformance.json.tmp",
        "pass CASES, BYTES, and NATIVE together, or none",
    ] {
        assert!(
            recipe.contains(needle),
            "the local preview renders both reports of one run ({needle}): {recipe}"
        );
    }

    let workflow =
        fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci workflow");
    let lane = workflow_job(&workflow, "conformance");
    assert!(
        lane.contains("name: conformance (${{ matrix.version }}, ${{ matrix.target }})")
            && lane.contains("target: [jvm, native, wasm]"),
        "every version exposes independent JVM, Native and Wasm rows: {lane}"
    );
    assert!(
        lane.contains(
            "just conformance-run \"$PWD/conformance-bin\" \"${{ matrix.version }}\" jvm-byte-equality.txt > pct.txt\n          bash scripts/conformance-badge.sh fields conformance pct.txt \"${{ matrix.version }}\"\n          bash scripts/conformance-badge.sh fields jvm-byte-equality jvm-byte-equality.txt \"${{ matrix.version }}\"\n"
        ),
        "each JVM row writes and validates both reports of one box run: {lane}"
    );
    assert!(
        lane.contains("name: pct-${{ matrix.version }}\n          path: pct.txt\n"),
        "each JVM row uploads its case report"
    );
    assert!(
        lane.contains(
            "name: jvm-byte-equality-${{ matrix.version }}\n          path: jvm-byte-equality.txt\n"
        ),
        "each JVM row uploads its byte report as a separate artifact"
    );
    assert!(
        lane.contains(
            "just native-conformance-run \"$PWD/conformance-bin\" \"${{ matrix.version }}\" native-pct.txt\n          bash scripts/conformance-badge.sh fields native-conformance native-pct.txt \"${{ matrix.version }}\"\n"
        ),
        "each Native row writes and validates its independent report: {lane}"
    );
    assert!(
        lane.contains("name: native-pct-${{ matrix.version }}\n          path: native-pct.txt\n"),
        "each Native row uploads its case report"
    );

    let release = workflow_job(&workflow, "release");
    assert!(
        release.contains("    if: github.ref == 'refs/heads/master'\n"),
        "only master publishes"
    );
    for needle in [
        "name: pct-${{ needs.versions.outputs.max }}\n",
        "name: jvm-byte-equality-${{ needs.versions.outputs.max }}\n",
        "name: native-pct-${{ needs.versions.outputs.max }}\n",
        "id: c\n",
        "run: bash scripts/conformance-badge.sh fields conformance pct.txt \"$MAX_VERSION\" >> \"$GITHUB_OUTPUT\"\n",
        "id: b\n",
        "run: bash scripts/conformance-badge.sh fields jvm-byte-equality jvm-byte-equality.txt \"$MAX_VERSION\" >> \"$GITHUB_OUTPUT\"\n",
        "id: n\n",
        "run: bash scripts/conformance-badge.sh fields native-conformance native-pct.txt \"$MAX_VERSION\" >> \"$GITHUB_OUTPUT\"\n",
        "filename: krusty-conformance.json\n          label: ${{ steps.c.outputs.label }}\n          message: ${{ steps.c.outputs.message }}\n          color: ${{ steps.c.outputs.color }}\n",
        "filename: krusty-jvm-byte-equality.json\n          label: ${{ steps.b.outputs.label }}\n          message: ${{ steps.b.outputs.message }}\n          color: ${{ steps.b.outputs.color }}\n",
        "filename: krusty-native-conformance.json\n          label: ${{ steps.n.outputs.label }}\n          message: ${{ steps.n.outputs.message }}\n          color: ${{ steps.n.outputs.color }}\n",
    ] {
        assert!(
            release.contains(needle),
            "the release publishes each max-version report to its own endpoint ({needle})"
        );
    }
    assert_eq!(
        workflow.matches("Schneegans/dynamic-badges-action").count(),
        release.matches("Schneegans/dynamic-badges-action").count(),
        "no job other than the master release publishes a badge"
    );
    assert_eq!(
        release.matches("Schneegans/dynamic-badges-action").count(),
        4,
        "the release publishes the Kotlin version, JVM conformance/bytes, and Native conformance badges"
    );
}

#[test]
fn readme_and_site_show_all_badges_with_distinct_endpoints() {
    let root = repo_root();
    let gist = "gist.githubusercontent.com%2Fqnox%2Fdec8149bc4f43b203d6cc9adc14f2026%2Fraw%2F";
    let readme = fs::read_to_string(root.join("README.md")).expect("read README");
    let site =
        fs::read_to_string(root.join("site/src/pages/index.astro")).expect("read landing page");
    assert!(
        site.contains(gist),
        "the site's badge helper uses the badge Gist"
    );
    for (endpoint, alt) in [
        (
            "krusty-conformance.json",
            "Kotlin conformance: share of applicable codegen/box cases whose box() returns OK",
        ),
        (
            "krusty-jvm-byte-equality.json",
            "JVM byte equality: matching leading .class bytes against kotlinc across the same codegen/box cases",
        ),
        (
            "krusty-native-conformance.json",
            "Native conformance: share of applicable Native codegen/box cases whose box() returns OK",
        ),
    ] {
        assert!(
            readme.contains(&format!("{gist}{endpoint}\" alt=\"{alt}\">")),
            "README shows the {endpoint} badge as {alt:?}"
        );
        assert!(
            site.contains(&format!("<img src={{badge(\"{endpoint}\")}} alt=\"{alt}\"")),
            "the site shows the {endpoint} badge as {alt:?}"
        );
    }
}
