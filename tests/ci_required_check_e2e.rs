//! The `ci-and-conformance` job in `.github/workflows/ci.yml`: the single non-matrix check a branch
//! ruleset can require in place of individual jobs, green only when every merge gate is green.

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

fn run_predicate(
    script: &str,
    ci: &str,
    klib: &str,
    kotlin_toolchain: &str,
    conformance: &str,
    gradle_plugin: &str,
    gradle: &str,
) -> std::process::Output {
    let mut child = Command::new("bash")
        .arg("-s")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("CI_RESULT", ci)
        .env("KLIB_RESULT", klib)
        .env("KOTLIN_TOOLCHAIN_RESULT", kotlin_toolchain)
        .env("CONFORMANCE_RESULT", conformance)
        .env("GRADLE_PLUGIN_RESULT", gradle_plugin)
        .env("GRADLE_RESULT", gradle)
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
fn aggregate_is_one_stable_always_scheduled_job_over_every_merge_gate() {
    let workflow = workflow();
    let aggregate = job(&workflow, AGGREGATE);
    assert!(
        aggregate.starts_with(&format!("  {AGGREGATE}:\n    name: {AGGREGATE}\n")),
        "the check context is pinned by an explicit name: {aggregate}"
    );
    assert!(
        aggregate.contains(
            "\n    needs: [ci, klib-semantics, kotlin-toolchain, conformance, build-gradle-plugin, gradle]\n"
        ),
        "the aggregate waits on every merge gate: {aggregate}"
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
            && aggregate.contains("KLIB_RESULT: ${{ needs.klib-semantics.result }}\n")
            && aggregate
                .contains("KOTLIN_TOOLCHAIN_RESULT: ${{ needs.kotlin-toolchain.result }}\n")
            && aggregate.contains("CONFORMANCE_RESULT: ${{ needs.conformance.result }}\n")
            && aggregate
                .contains("GRADLE_PLUGIN_RESULT: ${{ needs.build-gradle-plugin.result }}\n")
            && aggregate.contains("GRADLE_RESULT: ${{ needs.gradle.result }}\n"),
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
        job(&workflow, "kotlin-toolchain")
            .contains("        run: scripts/kotlin-toolchain/verify_recordings.sh\n"),
        "the Kotlin Toolchain gate re-records every corpus from the live references"
    );
    assert!(
        job(&workflow, "release").contains(
            "    needs: [ci, klib-semantics, conformance, build-gradle-plugin, gradle, versions, build-release]\n"
        ),
        "master release keeps its broader prerequisites"
    );
}

#[test]
fn aggregate_passes_only_when_every_merge_gate_succeeded() {
    let workflow = workflow();
    let script = run_script(job(&workflow, AGGREGATE));
    let run = |results: [&str; 6]| {
        run_predicate(
            &script, results[0], results[1], results[2], results[3], results[4], results[5],
        )
    };
    let success = ["success"; 6];
    let output = run(success);
    assert!(
        output.status.success(),
        "all-success aggregate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ci: success\nklib-semantics: success\nkotlin-toolchain: success\nconformance: success\nbuild-gradle-plugin: success\ngradle: success\n",
        "the log names every dependency result"
    );

    let gates = [
        "ci",
        "klib-semantics",
        "kotlin-toolchain",
        "conformance",
        "build-gradle-plugin",
        "gradle",
    ];
    for (gate, index) in gates.into_iter().zip(0..) {
        for result in ["failure", "cancelled", "skipped", ""] {
            let mut results = success;
            results[index] = result;
            let output = run(results);
            assert!(
                !output.status.success(),
                "{gate}={result:?} incorrectly passed"
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                "core CI, KLIB semantics, Kotlin Toolchain recordings, conformance, and every Gradle gate must succeed\n",
                "{gate}={result:?}"
            );
        }
    }
}

#[test]
fn gradle_matrix_caches_the_isolated_user_home_it_actually_consumes() {
    let workflow = workflow();
    let gradle = job(&workflow, "gradle");
    assert!(
        gradle.contains("          cache-provider: external\n"),
        "setup-gradle must not restore an unused default user home: {gradle}"
    );
    assert!(
        gradle.contains("          path: ${{ runner.temp }}/krusty-gradle-user-home/caches\n")
            && gradle.contains(
                "          KRUSTY_GRADLE_USER_HOME: ${{ runner.temp }}/krusty-gradle-user-home\n"
            ),
        "the cache path and harness user home must be the same directory: {gradle}"
    );
    for key_input in [
        "${{ matrix.gradle }}",
        "${{ matrix.kotlin }}",
        "${{ matrix.ksp }}",
        "crates/krusty-build/src/gradle.rs",
        "tools/krusty-gradle/**",
    ] {
        assert!(
            gradle.contains(key_input),
            "the integration cache key must track {key_input}: {gradle}"
        );
    }
    assert!(
        job(&workflow, "build-gradle-plugin").contains("          cache-provider: basic\n"),
        "the plugin build itself still uses setup-gradle caching"
    );
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
