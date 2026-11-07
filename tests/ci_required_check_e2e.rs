//! The `ci-and-conformance` job in `.github/workflows/ci.yml`: the single non-matrix check a branch
//! ruleset can require in place of individual jobs, green only when every merge gate is green.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const AGGREGATE: &str = "ci-and-conformance";

#[test]
fn every_klib_box_lane_scores_and_caches_its_exact_version() {
    let workflow = workflow();
    let conformance = job(&workflow, "conformance");
    for knob in [
        "KRUSTY_NATIVE_BYTE_REPORT:",
        "KRUSTY_WASM_JS_BYTE_REPORT:",
        "KRUSTY_WASM_WASI_BYTE_REPORT:",
        "KRUSTY_CLASS_DUMP_COMPILE_MISSING: \"1\"",
        "name: klib-byte-equality-${{ matrix.version }}-${{ matrix.target }}",
        "path: target/cache/klib-references",
        "box-klib-references-v1-${{ runner.os }}-${{ runner.arch }}-${{ matrix.target }}-${{ matrix.version }}-",
        "github.event_name == 'push' && github.ref == 'refs/heads/master' && steps.klib-recordings.outputs.cache-hit != 'true'",
    ] {
        assert!(conformance.contains(knob), "missing KLIB matrix contract {knob}");
    }
    assert!(!workflow.contains("build-klib-test-bins"));
    assert!(conformance.contains("target: [jvm, native, wasm-js, wasm-wasi]"));
}

#[test]
fn exact_klib_coverage_reuses_normal_units_in_every_platform_version() {
    let workflow = workflow();
    let producer = job(&workflow, "build-tool-bins");
    assert!(producer.contains("bin=$(just compiler-unit-tests)"));
    assert!(producer.contains("            compiler-unit-tests\n"));
    let matrix = job(&workflow, "conformance");
    let coverage = matrix
        .split_once("- name: enforce exact KLIB implementation coverage")
        .unwrap()
        .1
        .split("\n      - name:")
        .next()
        .unwrap();
    for required in [
        "if: always() && matrix.target != 'jvm'",
        "KRUSTY_REQUIRE_KLIB: \"1\"",
        "KRUSTY_REQUIRE_KLIB_IMPLEMENTATION_TARGETS: ${{ matrix.target }}",
        "export KRUSTY_LANGUAGE_VERSION=\"${{ matrix.version }}\"",
        "timeout 600s ./compiler-unit-tests",
        "klib_libraries::tests::implementation_coverage:: --nocapture",
    ] {
        assert!(
            coverage.contains(required),
            "missing exact-version unit coverage contract {required}"
        );
    }
    assert!(
        !coverage.contains("matrix.version !="),
        "the maximum Native version is checked too"
    );
    let recipe =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("justfile"))
            .unwrap();
    let recipe = recipe
        .split_once("\ncompiler-unit-tests:\n")
        .unwrap()
        .1
        .split("\n\n")
        .next()
        .unwrap();
    assert!(recipe.contains("scripts/cargo-test-binary.sh krusty -p krusty --lib"));
}

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

/// The aggregate's result variables, in the order its log names them.
const RESULT_VARIABLES: [&str; 9] = [
    "CI_RESULT",
    "LINT_RESULT",
    "PLUGIN_HOST_RESULT",
    "CONFORMANCE_RESULT",
    "GRADLE_PLUGIN_RESULT",
    "GRADLE_RESULT",
    "MAVEN_RESULT",
    "BUILD_TOOLS_RESULT",
    "BAZEL_RESULT",
];

fn run_predicate(script: &str, results: [&str; 9]) -> std::process::Output {
    let mut child = Command::new("bash")
        .arg("-s")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(RESULT_VARIABLES.into_iter().zip(results))
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
fn aggregate_passes_only_when_every_merge_gate_succeeded() {
    let workflow = workflow();
    let script = run_script(job(&workflow, AGGREGATE));
    let run = |results: [&str; 9]| run_predicate(&script, results);
    let success = ["success"; 9];
    let output = run(success);
    assert!(
        output.status.success(),
        "all-success aggregate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ci: success\nlint: success\nplugin-host: success\nconformance: success\nbuild-gradle-plugin: success\ngradle: success\nmaven: success\nbuild-tools: success\nbazel: success\n",
        "the log names every dependency result"
    );

    let gates = [
        "ci",
        "lint",
        "plugin-host",
        "conformance",
        "build-gradle-plugin",
        "gradle",
        "maven",
        "build-tools",
        "bazel",
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
                "lint, plugin host tests, coverage, conformance (including KLIB semantics), every Gradle gate, the Maven goal, every build-tools lane, and every Bazel lane must succeed\n",
                "{gate}={result:?}"
            );
        }
    }
}
