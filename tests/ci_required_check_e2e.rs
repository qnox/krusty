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
    lint: &str,
    conformance: &str,
    klib_conformance: &str,
    gradle_plugin: &str,
    gradle: &str,
    build_tools: &str,
) -> std::process::Output {
    let mut child = Command::new("bash")
        .arg("-s")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("CI_RESULT", ci)
        .env("LINT_RESULT", lint)
        .env("CONFORMANCE_RESULT", conformance)
        .env("KLIB_CONFORMANCE_RESULT", klib_conformance)
        .env("GRADLE_PLUGIN_RESULT", gradle_plugin)
        .env("GRADLE_RESULT", gradle)
        .env("BUILD_TOOLS_RESULT", build_tools)
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
            "\n    needs: [ci, lint, conformance, conformance-klib, build-gradle-plugin, gradle, build-tools]\n"
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
            && aggregate.contains("LINT_RESULT: ${{ needs.lint.result }}\n")
            && aggregate.contains("CONFORMANCE_RESULT: ${{ needs.conformance.result }}\n")
            && aggregate
                .contains("KLIB_CONFORMANCE_RESULT: ${{ needs.conformance-klib.result }}\n")
            && aggregate
                .contains("GRADLE_PLUGIN_RESULT: ${{ needs.build-gradle-plugin.result }}\n")
            && aggregate.contains("GRADLE_RESULT: ${{ needs.gradle.result }}\n")
            && aggregate.contains("BUILD_TOOLS_RESULT: ${{ needs.build-tools.result }}\n"),
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

    let ci = job(&workflow, "ci");
    let lint = job(&workflow, "lint");
    assert!(
        ci.contains("run: just coverage-gate\n")
            && !ci.contains("run: just ci\n")
            && !ci.contains("run: just lint\n")
            && !ci.contains("needs:"),
        "coverage must start on its own runner without waiting for lint: {ci}"
    );
    let coverage = ci
        .find("run: just coverage-gate\n")
        .expect("the coverage job runs its gate");
    let release_candidate = ci
        .find("- name: select the current master release candidate\n")
        .expect("coverage selects a release candidate");
    assert!(
        coverage < release_candidate
            && ci.contains(
                "release_candidate: ${{ steps.release_candidate.outputs.publish }}\n"
            )
            && ci.contains(
                "if: github.event_name == 'push' && github.ref == 'refs/heads/master'\n        id: release_candidate\n        run: scripts/release-publisher.sh check >> \"$GITHUB_OUTPUT\"\n"
            ),
        "release packaging must use the publisher's current-tip check only after successful coverage: {ci}"
    );
    assert!(
        lint.contains("run: just lint\n")
            && !lint.contains("coverage-gate")
            && !lint.contains("needs:"),
        "lint must run independently instead of extending the coverage critical path: {lint}"
    );

    let conformance = job(&workflow, "conformance");
    let klib_conformance = job(&workflow, "conformance-klib");
    assert!(
        conformance.contains("    needs: [build-conformance-bin, build-tool-bins]\n")
            && conformance.contains("      fail-fast: false\n")
            && conformance.contains(
                "        version: ${{ fromJson(needs.build-tool-bins.outputs.matrix) }}\n"
            )
            && conformance.contains("        target: [jvm]\n")
            && conformance.contains("    steps: &conformance-steps\n")
            && !conformance.contains("build-klib-test-bins"),
        "JVM conformance must start without waiting for the focused KLIB binaries: {conformance}"
    );
    assert!(
        klib_conformance.contains(
            "    needs: [build-conformance-bin, build-klib-test-bins, build-tool-bins]\n"
        ) && klib_conformance.contains("        target: [native, wasm]\n")
            && klib_conformance.contains("    steps: *conformance-steps\n"),
        "Native and Wasm wait for focused KLIB binaries while reusing the shared inventory: {klib_conformance}"
    );
    assert_eq!(
        workflow.matches("        target: [jvm]\n").count(),
        1,
        "the early matrix owns exactly the JVM rows"
    );
    assert_eq!(
        workflow.matches("        target: [native, wasm]\n").count(),
        1,
        "the focused-KLIB-dependent matrix owns exactly the Native and Wasm rows"
    );
    let conformance_build = job(&workflow, "build-conformance-bin");
    assert!(
        conformance_build.contains("bin=$(just conformance-bin)")
            && conformance_build.contains("name: conformance-bin\n")
            && !conformance_build.contains("needs:"),
        "one independent producer owns the shared conformance binary: {conformance_build}"
    );
    let klib_build = job(&workflow, "build-klib-test-bins");
    assert!(
        klib_build.contains("scripts/build-klib-test-binaries.sh \"$PWD\"")
            && klib_build.contains("name: klib-test-bins\n")
            && klib_build.contains("klib-e2e-tests")
            && klib_build.contains("klib-unit-tests")
            && !klib_build.contains("needs:"),
        "one independent producer owns both focused KLIB binaries: {klib_build}"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let klib_builder = fs::read_to_string(root.join("scripts/build-klib-test-binaries.sh"))
        .expect("read focused KLIB binary builder");
    assert!(
        klib_builder.contains("--profile klib-test")
            && klib_builder.contains("--lib --test klib_e2e")
            && klib_builder.contains("--features focused-klib-tests")
            && !klib_builder.contains("--profile gate")
            && !klib_builder.contains("--test e2e"),
        "the focused producer must use its cheaper profile without changing its two test targets: {klib_builder}"
    );
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read Cargo manifest");
    assert!(
        manifest.contains(
            "[profile.klib-test]\ninherits = \"gate\"\nlto = \"off\"\n\n[profile.klib-test.package.krusty]\nopt-level = 1\ncodegen-units = 256"
        ),
        "the focused producer profile must retain gate semantics and lower only krusty's optimization cost: {manifest}"
    );
    assert!(
        manifest.contains(
            "name = \"klib_e2e\"\npath = \"tests/klib_e2e.rs\"\nrequired-features = [\"focused-klib-tests\"]"
        ),
        "the focused target stays opt-in instead of duplicating tests in the ordinary suite: {manifest}"
    );
    let focused_klib =
        fs::read_to_string(root.join("tests/klib_e2e.rs")).expect("read focused KLIB test target");
    for module in [
        "klib_class_surface_e2e",
        "klib_container_e2e",
        "klib_semantic_e2e",
        "outcome_lists_guard_e2e",
        "value_class_declaration_e2e",
        "native_value_classes_e2e",
    ] {
        assert!(
            focused_klib.contains(&format!("mod {module};")),
            "the focused target preserves the Native row's existing {module} inventory: {focused_klib}"
        );
    }
    let tool_build = job(&workflow, "build-tool-bins");
    assert!(
        tool_build.contains("cargo build --profile gate -p krusty-cli")
            && tool_build.contains("bin=$(just krusty-build-tests)")
            && tool_build.contains("name: tool-bins\n")
            && tool_build.contains("matrix: ${{ steps.versions.outputs.matrix }}\n")
            && tool_build.contains("max: ${{ steps.versions.outputs.max }}\n")
            && tool_build.contains("id: versions\n")
            && tool_build.contains("echo \"matrix=$(just matrix-json)\"")
            && tool_build.contains("echo \"max=$(just max-version)\"")
            && !tool_build.contains("needs:"),
        "one independent producer owns the compiler, Gradle test binaries, and version inventory: {tool_build}"
    );
    assert!(
        !workflow.contains("\n  versions:\n"),
        "the version inventory must not consume a separate runner"
    );
    for artifact in ["conformance-bin", "klib-test-bins", "tool-bins"] {
        assert!(
            conformance.contains(&format!("          name: {artifact}\n")),
            "the shared conformance inventory consumes {artifact}: {conformance}"
        );
    }
    assert!(
        !workflow.contains("\n  klib-semantics:\n"),
        "KLIB semantics are ordinary conformance coverage, not a standalone job"
    );
    for needle in [
        "if: matrix.target == 'native' && matrix.version == needs.build-tool-bins.outputs.max && (success() || steps.native_box.outcome == 'failure' || steps.native_focused.outcome == 'failure')",
        "timeout 600s ./klib-e2e-tests klib_ --nocapture",
        "timeout 600s ./klib-e2e-tests value_class_declaration_e2e --nocapture",
        "native_value_classes_e2e::a_value_class_needs_no_jvm_inline_on_native --nocapture",
        "timeout 600s ./klib-unit-tests klib_ --nocapture",
    ] {
        assert!(
            conformance.contains(needle),
            "the maximum-version Native row must retain focused KLIB coverage ({needle}): {conformance}"
        );
    }
    assert!(
        conformance.contains(
            "if: matrix.target == 'wasm' || (matrix.target == 'native' && matrix.version != needs.build-tool-bins.outputs.max)"
        ) && conformance.contains(
            "klib_libraries::tests::implementation_coverage:: --nocapture"
        ),
        "every non-maximum Native/Wasm matrix row must run the exact implementation ratchet: {conformance}"
    );
    let focused = conformance
        .find("- name: run focused KLIB semantics and Native oracle coverage")
        .expect("Native conformance retains focused KLIB coverage");
    let enforcement = conformance
        .find("- name: enforce native box conformance")
        .expect("Native conformance enforces its corpus result");
    assert!(
        focused < enforcement,
        "a corpus failure must not skip the independently folded KLIB coverage"
    );
    let release_build = job(&workflow, "build-release");
    assert!(
        release_build.contains("    needs: [ci]\n")
            && release_build.contains(
                "    if: github.ref == 'refs/heads/master' && needs.ci.outputs.release_candidate == 'true'\n"
            )
            && release_build.contains("      max-parallel: 2\n"),
        "only the current covered master commit may occupy at most two release runners: {release_build}"
    );
    let release = job(&workflow, "release");
    assert!(
        release.contains(
            "    needs: [ci, lint, conformance, conformance-klib, build-gradle-plugin, gradle, build-tools, build-tool-bins, build-release]\n"
        ) && release.contains("run: scripts/release-publisher.sh check >> \"$GITHUB_OUTPUT\"\n"),
        "master release keeps its broader prerequisites and rechecks under the publisher lock: {release}"
    );
    assert_eq!(
        workflow
            .matches("scripts/release-publisher.sh check >> \"$GITHUB_OUTPUT\"")
            .count(),
        2,
        "coverage filters stale packaging early and release independently rechecks before publishing"
    );
}

#[test]
fn aggregate_passes_only_when_every_merge_gate_succeeded() {
    let workflow = workflow();
    let script = run_script(job(&workflow, AGGREGATE));
    let run = |results: [&str; 7]| {
        run_predicate(
            &script, results[0], results[1], results[2], results[3], results[4], results[5],
            results[6],
        )
    };
    let success = ["success"; 7];
    let output = run(success);
    assert!(
        output.status.success(),
        "all-success aggregate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ci: success\nlint: success\nconformance: success\nKLIB conformance: success\nbuild-gradle-plugin: success\ngradle: success\nbuild-tools: success\n",
        "the log names every dependency result"
    );

    let gates = [
        "ci",
        "lint",
        "conformance",
        "KLIB conformance",
        "build-gradle-plugin",
        "gradle",
        "build-tools",
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
                "lint, coverage, conformance (including KLIB semantics), every Gradle gate, and every build-tools lane must succeed\n",
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
fn coverage_build_tools_gradle_home_is_not_restored_with_target() {
    let workflow = workflow();
    let ci = job(&workflow, "ci");
    assert!(
        ci.contains(
            "          KRUSTY_BUILD_TOOLS_GRADLE_USER_HOME: ${{ runner.temp }}/krusty-build-tools-gradle-user-home\n"
        ),
        "the Build Tools oracle's mutable Gradle state must stay outside rust-cache's target archive: {ci}"
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
