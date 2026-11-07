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
const RESULT_VARIABLES: [&str; 10] = [
    "CI_RESULT",
    "LINT_RESULT",
    "PLUGIN_HOST_RESULT",
    "CONFORMANCE_RESULT",
    "KLIB_CONFORMANCE_RESULT",
    "GRADLE_PLUGIN_RESULT",
    "GRADLE_RESULT",
    "MAVEN_RESULT",
    "BUILD_TOOLS_RESULT",
    "BAZEL_RESULT",
];

fn run_predicate(script: &str, results: [&str; 10]) -> std::process::Output {
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
    let run = |results: [&str; 10]| run_predicate(&script, results);
    let success = ["success"; 10];
    let output = run(success);
    assert!(
        output.status.success(),
        "all-success aggregate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ci: success\nlint: success\nplugin-host: success\nconformance: success\nKLIB conformance: success\nbuild-gradle-plugin: success\ngradle: success\nmaven: success\nbuild-tools: success\nbazel: success\n",
        "the log names every dependency result"
    );

    let gates = [
        "ci",
        "lint",
        "plugin-host",
        "conformance",
        "KLIB conformance",
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
