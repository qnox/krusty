//! The `ci-and-conformance` job in `.github/workflows/ci.yml`: the single non-matrix check a branch
//! ruleset can require in place of `ci`, green only when `ci` and every conformance lane are green.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const AGGREGATE: &str = "ci-and-conformance";

fn workflow() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci workflow")
}

/// The text of one top-level job, from its `  <id>:` line up to the next job.
fn job<'a>(workflow: &'a str, id: &str) -> &'a str {
    let header = format!("\n  {id}:\n");
    let start = workflow
        .find(&header)
        .unwrap_or_else(|| panic!("ci.yml has a `{id}` job"))
        + 1;
    let rest = &workflow[start..];
    let mut offset = 0;
    for (index, line) in rest.split_inclusive('\n').enumerate() {
        let next_job = line
            .strip_prefix("  ")
            .is_some_and(|key| !key.starts_with([' ', '#', '\n']));
        if index > 0 && next_job {
            return &rest[..offset];
        }
        offset += line.len();
    }
    rest
}

/// The body of the job's only `run: |` step, de-indented.
fn run_script(job: &str) -> String {
    let marker = "run: |\n";
    let start = job.find(marker).expect("the aggregate has a run step") + marker.len();
    assert_eq!(
        job.matches(marker).count(),
        1,
        "the aggregate has exactly one script step: {job}"
    );
    let mut script = String::new();
    for line in job[start..].lines() {
        if line.trim().is_empty() {
            script.push('\n');
            continue;
        }
        let Some(stripped) = line.strip_prefix("          ") else {
            break;
        };
        script.push_str(stripped);
        script.push('\n');
    }
    script
}

fn run_predicate(script: &str, ci: &str, conformance: &str) -> std::process::Output {
    let mut child = Command::new("bash")
        .arg("-s")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("CI_RESULT", ci)
        .env("CONFORMANCE_RESULT", conformance)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash");
    child
        .stdin
        .take()
        .expect("bash stdin")
        .write_all(script.as_bytes())
        .expect("write aggregate script");
    child.wait_with_output().expect("wait for bash")
}

#[test]
fn aggregate_is_one_stable_always_scheduled_job_over_ci_and_the_whole_matrix() {
    let workflow = workflow();
    let aggregate = job(&workflow, AGGREGATE);
    assert!(
        aggregate.starts_with(&format!("  {AGGREGATE}:\n    name: {AGGREGATE}\n")),
        "the check context is pinned by an explicit name: {aggregate}"
    );
    assert!(
        aggregate.contains("\n    needs: [ci, conformance]\n"),
        "the aggregate waits on `ci` and the complete conformance matrix only: {aggregate}"
    );
    assert!(
        aggregate.contains("\n    if: always()\n"),
        "a skipped aggregate would satisfy a required check, so it is always scheduled: {aggregate}"
    );
    assert!(
        !aggregate.contains("strategy:") && !aggregate.contains("matrix."),
        "the aggregate is not a matrix job, so its context does not depend on versions: {aggregate}"
    );
    assert!(
        aggregate.contains("CI_RESULT: ${{ needs.ci.result }}\n")
            && aggregate.contains("CONFORMANCE_RESULT: ${{ needs.conformance.result }}\n"),
        "the step reads each dependency's own result: {aggregate}"
    );
    assert_eq!(
        workflow.matches(&format!("name: {AGGREGATE}\n")).count(),
        1,
        "exactly one job carries the aggregate context"
    );

    let triggers = &workflow[workflow.find("\non:\n").expect("workflow triggers")
        ..workflow
            .find("\npermissions:\n")
            .expect("permissions block")];
    assert_eq!(
        triggers, "\non:\n  pull_request:\n  merge_group:\n  push:\n    branches: [master]\n",
        "the aggregate runs for pull requests, merge groups, and master pushes"
    );

    let conformance = job(&workflow, "conformance");
    assert!(
        conformance.contains("    needs: [build-shared-bins, versions]\n")
            && conformance.contains("      fail-fast: false\n")
            && conformance
                .contains("        version: ${{ fromJson(needs.versions.outputs.matrix) }}\n"),
        "conformance still expands every manifest version without cancelling siblings: {conformance}"
    );
    assert!(
        job(&workflow, "release").contains(
            "    needs: [ci, klib-semantics, conformance, build-gradle-plugin, gradle, versions, build-release]\n"
        ),
        "master release keeps its broader prerequisites"
    );
}

#[test]
fn aggregate_passes_only_when_ci_and_conformance_both_succeeded() {
    let workflow = workflow();
    let script = run_script(job(&workflow, AGGREGATE));
    let results = ["success", "failure", "cancelled", "skipped", ""];
    for ci in results {
        for conformance in results {
            let output = run_predicate(&script, ci, conformance);
            let passed = output.status.success();
            assert_eq!(
                passed,
                ci == "success" && conformance == "success",
                "ci={ci:?} conformance={conformance:?} stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert_eq!(
                stdout,
                format!("ci: {ci}\nconformance: {conformance}\n"),
                "the log names each dependency result"
            );
            if !passed {
                assert_eq!(
                    String::from_utf8_lossy(&output.stderr),
                    "ci and every conformance lane must succeed\n",
                    "ci={ci:?} conformance={conformance:?}"
                );
            }
        }
    }
}

#[test]
fn harness_doc_names_the_aggregate_context_for_the_post_integration_ruleset_switch() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let doc = fs::read_to_string(root.join("docs/TEST_HARNESS.md")).expect("read TEST_HARNESS.md");
    let section = &doc[doc
        .find("\n## Required Check\n")
        .expect("TEST_HARNESS.md documents the required check")..];
    let section = &section[..section[1..]
        .find("\n## ")
        .map_or(section.len(), |end| end + 1)];
    for needle in [
        &format!("`{AGGREGATE}`") as &str,
        "rulesets/19534763",
        "krusty-conformance.json",
    ] {
        assert!(
            section.contains(needle),
            "the required-check section mentions {needle}: {section}"
        );
    }
}
