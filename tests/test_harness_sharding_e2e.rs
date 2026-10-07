use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn planner_arg(input: &str, shards: &str) -> std::process::Output {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("libtest-shard-plan.sh");
    let mut child = Command::new("bash")
        .arg(script)
        .arg(shards)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libtest shard planner");
    child
        .stdin
        .take()
        .expect("planner stdin")
        .write_all(input.as_bytes())
        .expect("write planner input");
    child.wait_with_output().expect("wait for shard planner")
}

fn planner(input: &str, shards: usize) -> std::process::Output {
    planner_arg(input, &shards.to_string())
}

fn compact_skip_patterns(plan: &Path, listing: &Path, shard: usize) -> std::process::Output {
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("libtest-shards.sh");
    Command::new("bash")
        .args([
            "-c",
            "source \"$1\"; libtest_shard_skip_patterns \"$2\" \"$3\" \"$4\"",
            "shard-pattern-test",
        ])
        .arg(helper)
        .arg(plan)
        .arg(listing)
        .arg(shard.to_string())
        .output()
        .expect("generate compact shard skip patterns")
}

fn without_phase_timing(stderr: &str) -> String {
    let mut kept = String::new();
    for line in stderr.lines() {
        if line.contains(": phase") {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

fn parse_plan(output: &[u8]) -> Vec<(usize, usize, String)> {
    String::from_utf8(output.to_vec())
        .expect("planner output is UTF-8")
        .lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let shard = fields.next().expect("shard field").parse().unwrap();
            let tests = fields.next().expect("test-count field").parse().unwrap();
            let module = fields.next().expect("module field").to_owned();
            assert!(fields.next().is_none(), "unexpected planner field: {line}");
            (shard, tests, module)
        })
        .collect()
}

const LISTING: &str = "\
gamma::one: test
alpha::one: test
epsilon::one: test
beta::one: test
alpha::two: test
delta::one: test
gamma::two: test
alpha::three: test
beta::two: test

9 tests, 0 benchmarks
";

#[test]
fn shard_planner_balances_whole_modules_and_covers_every_test_once() {
    let output = planner(LISTING, 2);
    assert!(
        output.status.success(),
        "planner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan = parse_plan(&output.stdout);

    let expected = BTreeMap::from([
        ("alpha".to_owned(), 3usize),
        ("beta".to_owned(), 2),
        ("delta".to_owned(), 1),
        ("epsilon".to_owned(), 1),
        ("gamma".to_owned(), 2),
    ]);
    let actual = plan
        .iter()
        .map(|(_, tests, module)| (module.clone(), *tests))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    assert_eq!(plan.len(), expected.len(), "a module was assigned twice");

    let mut loads = [0usize; 2];
    for (shard, tests, _) in &plan {
        assert!(*shard < loads.len());
        loads[*shard] += tests;
    }
    assert_eq!(loads.iter().sum::<usize>(), 9);
    assert!(loads[0].abs_diff(loads[1]) <= 3);
}

#[test]
fn shard_plan_is_stable_when_libtest_listing_order_changes() {
    let forward = planner(LISTING, 3);
    assert!(forward.status.success());

    let reversed = LISTING.lines().rev().collect::<Vec<_>>().join("\n");
    let reverse = planner(&reversed, 3);
    assert!(reverse.status.success());
    assert_eq!(forward.stdout, reverse.stdout);

    let assigned = parse_plan(&forward.stdout)
        .into_iter()
        .map(|(_, _, module)| module)
        .collect::<BTreeSet<_>>();
    assert_eq!(assigned.len(), 5);
}

#[test]
fn shard_planner_rejects_noncanonical_or_nonpositive_counts() {
    for invalid in ["", "0", "00", "01", "-1", "two"] {
        let output = planner_arg(LISTING, invalid);
        assert!(
            !output.status.success(),
            "planner accepted invalid shard count {invalid:?}"
        );
    }
}

#[test]
fn canonical_gate_defaults_bound_processes_and_partition_e2e() {
    let defaults = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("test-gate-defaults.sh");
    let output = Command::new("bash")
        .args([
            "-c",
            "unset KRUSTY_TEST_TIMEOUT_SECONDS KRUSTY_CONFORMANCE_TIMEOUT_SECONDS KRUSTY_E2E_TIMEOUT_SECONDS KRUSTY_CONFORMANCE_SHARDS KRUSTY_E2E_SHARDS; source \"$1\"; printf '%s\\n' \"$KRUSTY_TEST_TIMEOUT_SECONDS\" \"$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS\" \"$KRUSTY_E2E_TIMEOUT_SECONDS\" \"$KRUSTY_CONFORMANCE_SHARDS\" \"$KRUSTY_E2E_SHARDS\"",
            "gate-default-test",
        ])
        .arg(defaults)
        .output()
        .expect("read canonical gate defaults");
    assert!(
        output.status.success(),
        "gate defaults failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values = String::from_utf8(output.stdout)
        .expect("gate defaults are UTF-8")
        .lines()
        .map(|value| value.parse::<u64>().expect("numeric gate default"))
        .collect::<Vec<_>>();
    assert_eq!(values, [120, 120, 120, 4, 22]);
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_runner_enforces_its_configured_deadline() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = root.join("scripts").join("conformance-run.sh");
    let temp = std::env::temp_dir().join(format!(
        "krusty-conformance-run-deadline-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create conformance deadline test directory");
    let binary = temp.join("conformance-bin");
    fs::write(&binary, "#!/usr/bin/env bash\nsleep 10\n")
        .expect("write delayed conformance fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make delayed conformance fixture executable");

    let started = std::time::Instant::now();
    let output = Command::new("bash")
        .arg(runner)
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "1")
        .output()
        .expect("run delayed conformance fixture");
    let elapsed = started.elapsed();

    assert_eq!(output.status.code(), Some(124));
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).expect("deadline stderr is UTF-8");
    assert!(
        stderr.contains("conformance-run: phase start box-shard-1-of-4"),
        "missing shard phase start: {stderr}"
    );
    assert!(
        !stderr.contains("box-shard-2-of-4"),
        "a later shard started after the deadline: {stderr}"
    );
    assert_eq!(
        without_phase_timing(&stderr),
        "conformance-run: timed out after 1s: Kotlin 2.4.10, shard 1/4\n"
    );
    assert!(elapsed.as_secs() < 5, "deadline took {elapsed:?}");
    fs::remove_dir_all(temp).expect("remove conformance deadline test directory");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_runner_preserves_the_report_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = root.join("scripts").join("conformance-run.sh");
    let temp = std::env::temp_dir().join(format!(
        "krusty-conformance-run-report-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create conformance report test directory");
    let binary = temp.join("conformance-bin");
    fs::write(
        &binary,
        "#!/usr/bin/env bash\nprintf '%s|%s|%s|%s|%s|%s/%s\\n' \"$KRUSTY_LANGUAGE_VERSION\" \"$KRUSTY_KOTLINC\" \"$KRUSTY_KOTLIN_BOX_DIR\" \"$1\" \"$2\" \"$KRUSTY_CONFORMANCE_SHARD_INDEX\" \"$KRUSTY_CONFORMANCE_SHARD_COUNT\" >&2\nprintf '62.5 5 8\\n' >\"$KRUSTY_CONFORMANCE_REPORT\"\n",
    )
    .expect("write reporting conformance fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make reporting conformance fixture executable");

    let output = Command::new("bash")
        .arg(runner)
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_KOTLINC", "/bin/reference-kotlinc")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .output()
        .expect("run reporting conformance fixture");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"62.5 20 32\n");
    let stderr = String::from_utf8(output.stderr).expect("report stderr is UTF-8");
    assert!(
        stderr.contains("conformance-run: phase start box-shard-1-of-4"),
        "missing first shard phase: {stderr}"
    );
    assert!(
        stderr.contains("conformance-run: phase start box-shard-4-of-4"),
        "missing last shard phase: {stderr}"
    );
    assert!(
        stderr.contains("conformance-run: phase total "),
        "missing phase total: {stderr}"
    );
    assert_eq!(
        without_phase_timing(&stderr),
        format!(
            "2.4.10|/bin/reference-kotlinc|{0}|kotlin_codegen_box_conformance|--nocapture|0/4\n\
             2.4.10|/bin/reference-kotlinc|{0}|kotlin_codegen_box_conformance|--nocapture|1/4\n\
             2.4.10|/bin/reference-kotlinc|{0}|kotlin_codegen_box_conformance|--nocapture|2/4\n\
             2.4.10|/bin/reference-kotlinc|{0}|kotlin_codegen_box_conformance|--nocapture|3/4\n",
            temp.display(),
        )
    );
    fs::remove_dir_all(temp).expect("remove conformance report test directory");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_runner_finishes_every_shard_after_a_failing_one() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = root.join("scripts").join("conformance-run.sh");
    let temp = std::env::temp_dir().join(format!(
        "krusty-conformance-run-failing-shard-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create failing-shard test directory");
    let binary = temp.join("conformance-bin");
    // Every shard reports; the second one then fails its expected-failure check.
    fs::write(
        &binary,
        "#!/usr/bin/env bash\nprintf 'shard %s\\n' \"$KRUSTY_CONFORMANCE_SHARD_INDEX\" >&2\nprintf '50.0 1 2\\n' >\"$KRUSTY_CONFORMANCE_REPORT\"\n[ \"$KRUSTY_CONFORMANCE_SHARD_INDEX\" != 1 ] || exit 101\n",
    )
    .expect("write failing-shard conformance fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make failing-shard conformance fixture executable");

    let output = Command::new("bash")
        .arg(runner)
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_KOTLINC", "/bin/reference-kotlinc")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .output()
        .expect("run failing-shard conformance fixture");

    assert_eq!(output.status.code(), Some(101));
    assert_eq!(output.stdout, b"50.0 4 8\n");
    let stderr = String::from_utf8(output.stderr).expect("failing-shard stderr is UTF-8");
    assert_eq!(
        without_phase_timing(&stderr),
        "shard 0\nshard 1\nshard 2\nshard 3\n"
    );
    fs::remove_dir_all(temp).expect("remove failing-shard test directory");
}

#[test]
fn shard_listing_runs_through_the_deadline_helper() {
    let executable = std::env::current_exe().expect("current e2e test executable");
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("libtest-shards.sh");
    let temp = std::env::temp_dir().join(format!(
        "krusty-libtest-list-deadline-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create deadline test directory");
    let listing = temp.join("e2e.list");
    let plan = temp.join("e2e.plan");
    let marker = temp.join("deadline.marker");
    let output = Command::new("bash")
        .args([
            "-c",
            "source \"$1\"; MARKER=\"$2\"; run_with_deadline() { printf '%s\\n' \"$1\" >\"$MARKER\"; shift; \"$@\"; }; libtest_write_shard_plan \"$3\" 2 \"$4\" \"$5\" 17",
            "shard-deadline-test",
        ])
        .arg(helper)
        .arg(&marker)
        .arg(&executable)
        .arg(&listing)
        .arg(&plan)
        .output()
        .expect("run shard planner with deadline probe");
    assert!(
        output.status.success(),
        "deadline probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "17\n");
    assert!(!fs::read_to_string(&plan).unwrap().is_empty());
    fs::remove_dir_all(temp).expect("remove deadline test directory");
}

#[test]
fn real_e2e_libtest_filters_select_exactly_the_planned_shards() {
    const SHARDS: usize = 22;
    let executable = std::env::current_exe().expect("current e2e test executable");
    let listing = Command::new(&executable)
        .args(["--list", "--format", "terse"])
        .output()
        .expect("list e2e tests");
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).expect("libtest listing is UTF-8");

    let planned = planner(&listing, SHARDS);
    assert!(
        planned.status.success(),
        "planner failed: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let plan = parse_plan(&planned.stdout);
    let expected_tests = listing
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut selected_tests = BTreeSet::new();
    let temp = std::env::temp_dir().join(format!(
        "krusty-libtest-shards-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    fs::create_dir_all(&temp).expect("create shard test directory");
    let listing_path = temp.join("e2e.list");
    let plan_path = temp.join("e2e.plan");
    fs::write(&listing_path, &listing).expect("write libtest listing");
    fs::write(&plan_path, &planned.stdout).expect("write shard plan");

    for shard in 0..SHARDS {
        let expected = plan
            .iter()
            .filter(|(assigned, _, _)| *assigned == shard)
            .map(|(_, tests, _)| tests)
            .sum::<usize>();
        let patterns = compact_skip_patterns(&plan_path, &listing_path, shard);
        assert!(
            patterns.status.success(),
            "skip-pattern helper failed for shard {shard}: {}",
            String::from_utf8_lossy(&patterns.stderr)
        );
        let patterns = String::from_utf8(patterns.stdout).expect("skip patterns are UTF-8");
        let patterns = patterns.lines().collect::<Vec<_>>();
        let argument_bytes = patterns
            .iter()
            .map(|pattern| pattern.len() + 8)
            .sum::<usize>();
        assert!(
            argument_bytes < 64 * 1024,
            "shard {shard} skip arguments use {argument_bytes} bytes"
        );

        let mut command = Command::new(&executable);
        command.args(["--list", "--format", "terse"]);
        for pattern in patterns {
            command.args(["--skip", pattern]);
        }
        let selected = command.output().expect("list one planned shard");
        assert!(selected.status.success());
        let selected = String::from_utf8(selected.stdout).expect("shard listing is UTF-8");
        let selected = selected
            .lines()
            .filter_map(|line| line.strip_suffix(": test"))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            selected.len(),
            expected,
            "libtest selection drift in shard {shard}"
        );
        for test in selected {
            assert!(
                selected_tests.insert(test.clone()),
                "test selected by more than one shard: {test}"
            );
        }
    }
    assert_eq!(
        selected_tests, expected_tests,
        "shard union changed the test set"
    );
    fs::remove_dir_all(temp).expect("remove shard test directory");
}

fn target_hygiene(function: &str, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("target-hygiene.sh");
    let mut command = Command::new("bash");
    command
        .args([
            "-c",
            "source \"$1\"; fn=\"$2\"; shift 2; \"$fn\" \"$@\"",
            "target-hygiene-test",
        ])
        .arg(helper)
        .arg(function)
        .args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run target hygiene helper")
}

#[test]
fn target_hygiene_rejects_a_profile_name_used_as_the_target_dir() {
    // `gate` is a Cargo *profile*. `--target-dir target/gate` / `CARGO_TARGET_DIR=target/gate` builds
    // the dev profile into `target/gate/debug`, which nothing ever reuses or cleans.
    for bad in ["target/gate", "/abs/target/gate/", "target/gate-trace"] {
        let output = target_hygiene("target_hygiene_reject_profile_dir", &[bad], &[]);
        assert_eq!(output.status.code(), Some(2), "{bad} must be rejected");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--profile gate"),
            "hint missing for {bad}: {stderr}"
        );
    }
    for good in [
        "target",
        "/abs/target",
        "target/kt-2.4.10",
        "target/coverage-run",
    ] {
        let output = target_hygiene("target_hygiene_reject_profile_dir", &[good], &[]);
        assert!(
            output.status.success(),
            "{good} wrongly rejected: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn target_hygiene_prunes_orphan_objects_and_nested_profile_dirs() {
    let root = std::env::temp_dir().join(format!("krusty-target-hygiene-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let deps = root.join("gate").join("deps");
    let nested = root.join("gate").join("debug").join("deps");
    let debug_deps = root.join("debug").join("deps");
    for dir in [&deps, &nested, &debug_deps] {
        fs::create_dir_all(dir).expect("create fake target tree");
    }
    let old_obj = deps.join("krusty-0123456789abcdef.abc.def.rcgu.o");
    let fresh_obj = deps.join("krusty-fedcba9876543210.abc.def.rcgu.o");
    let rlib = deps.join("libkrusty-0123456789abcdef.rlib");
    let old_debug_obj = debug_deps.join("survey-0123456789abcdef.abc.def.rcgu.o");
    let nested_obj = nested.join("krusty-0123456789abcdef.abc.def.rcgu.o");
    for file in [&old_obj, &fresh_obj, &rlib, &old_debug_obj, &nested_obj] {
        fs::write(file, b"x").expect("write fake artifact");
    }
    for old in [&old_obj, &rlib, &old_debug_obj] {
        let status = Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(old)
            .status()
            .expect("age fake artifact");
        assert!(status.success());
    }

    let output = target_hygiene("target_hygiene_prune", &[root.to_str().unwrap(), "60"], &[]);
    assert!(
        output.status.success(),
        "prune failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!old_obj.exists(), "old orphan object must be pruned");
    assert!(
        !old_debug_obj.exists(),
        "orphan objects are pruned under every profile dir"
    );
    assert!(
        fresh_obj.exists(),
        "objects younger than the cutoff may belong to a live rustc"
    );
    assert!(
        rlib.exists(),
        "only *.rcgu.o temporaries are pruned, never finished artifacts"
    );
    assert!(
        !root.join("gate").join("debug").exists(),
        "nested profile dir is junk by construction"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gate/debug"),
        "removal must be reported: {stderr}"
    );
    assert!(
        stderr.contains("2 orphan"),
        "pruned object count must be reported: {stderr}"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn run_tests_wires_target_hygiene_before_building() {
    let script = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("run-tests.sh"))
        .expect("read run-tests.sh");
    let source = script
        .find("scripts/target-hygiene.sh")
        .expect("run-tests.sh sources target-hygiene.sh");
    let reject = script
        .find("target_hygiene_reject_profile_dir")
        .expect("run-tests.sh rejects profile-named target dirs");
    let prune = script
        .find("target_hygiene_prune")
        .expect("run-tests.sh prunes orphan objects");
    let first_build = script.find("cargo build").expect("run-tests.sh builds");
    assert!(
        source < reject && reject < first_build,
        "reject must run before the first cargo build"
    );
    assert!(
        prune < first_build,
        "prune must run before the first cargo build"
    );
}

#[cfg(unix)]
fn regression_fixture(name: &str, body: &str) -> (PathBuf, PathBuf) {
    let temp = std::env::temp_dir().join(format!(
        "krusty-conformance-regressions-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(temp.join("bin")).expect("create regressions fixture directory");
    let binary = temp.join("conformance-bin");
    fs::write(&binary, body).expect("write regressions fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make regressions fixture executable");
    let just_stub = temp.join("bin").join("just");
    fs::write(&just_stub, "#!/bin/sh\nexit 99\n").expect("write just stub");
    fs::set_permissions(&just_stub, fs::Permissions::from_mode(0o755))
        .expect("make just stub executable");
    (temp, binary)
}

#[cfg(unix)]
fn regression_threads() -> String {
    // Probe with the same pipeline scripts/conformance-regressions.sh uses: nproc is GNU
    // coreutils, getconf is POSIX and present on macOS, sysctl is the last resort.
    let probed = Command::new("sh")
        .args([
            "-c",
            "nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu",
        ])
        .output()
        .expect("probe the CPU count");
    assert!(
        probed.status.success(),
        "CPU count probe failed: {probed:?}"
    );
    let n = String::from_utf8(probed.stdout)
        .expect("CPU count is UTF-8")
        .trim()
        .parse::<u32>()
        .expect("CPU count is a number");
    n.min(4).to_string()
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_regressions_skip_the_box_suite() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture(
        "skip",
        "#!/usr/bin/env bash\nprintf 'bin=%s\\n' \"$KRUSTY_BIN\"\nprintf '%s\\n' \"$@\"\n",
    );
    let sibling = temp.join("krusty");
    fs::write(&sibling, "#!/bin/sh\nexit 0\n").expect("write sibling cli");
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755))
        .expect("make sibling cli executable");

    let output = Command::new("bash")
        .arg(root.join("scripts").join("conformance-regressions.sh"))
        .arg(&binary)
        .arg("2.4.10")
        .env_remove("KRUSTY_BIN")
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", temp.join("bin").display()),
        )
        .output()
        .expect("run conformance regressions");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("regressions stdout is UTF-8");
    assert_eq!(
        stdout,
        format!(
            "bin={}\n--skip\nkotlin_codegen_box_conformance\n--test-threads\n{}\n",
            sibling.display(),
            regression_threads(),
        ),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_regressions_recipe_runs_the_script() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture(
        "recipe",
        "#!/usr/bin/env bash\nprintf 'bin=%s\\n' \"$KRUSTY_BIN\"\nprintf '%s\\n' \"$@\"\n",
    );
    let sibling = temp.join("krusty");
    fs::write(&sibling, "#!/bin/sh\nexit 0\n").expect("write sibling cli");
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755))
        .expect("make sibling cli executable");

    let output = Command::new("just")
        .current_dir(&root)
        .arg("--justfile")
        .arg(root.join("justfile"))
        .arg("conformance-regressions")
        .arg(&binary)
        .arg("2.4.10")
        .env_remove("KRUSTY_BIN")
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .output()
        .expect("run conformance-regressions recipe");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("recipe stdout is UTF-8");
    assert_eq!(
        stdout,
        format!(
            "bin={}\n--skip\nkotlin_codegen_box_conformance\n--test-threads\n{}\n",
            sibling.display(),
            regression_threads(),
        ),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_regressions_keep_an_explicit_cli() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture(
        "cli",
        "#!/usr/bin/env bash\nprintf 'bin=%s\\n' \"$KRUSTY_BIN\"\n",
    );
    let output = Command::new("bash")
        .arg(root.join("scripts").join("conformance-regressions.sh"))
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_BIN", "/opt/krusty")
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", temp.join("bin").display()),
        )
        .output()
        .expect("run conformance regressions");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"bin=/opt/krusty\n");
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_regressions_fail_when_the_suite_fails() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture("fail", "#!/usr/bin/env bash\nexit 7\n");
    let output = Command::new("bash")
        .arg(root.join("scripts").join("conformance-regressions.sh"))
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", temp.join("bin").display()),
        )
        .output()
        .expect("run failing conformance regressions");
    assert_eq!(output.status.code(), Some(7));
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_regressions_enforce_the_deadline() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture("deadline", "#!/usr/bin/env bash\nsleep 10\n");
    let started = std::time::Instant::now();
    let output = Command::new("bash")
        .arg(root.join("scripts").join("conformance-regressions.sh"))
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "1")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", temp.join("bin").display()),
        )
        .output()
        .expect("run delayed conformance regressions");
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(
        String::from_utf8(output.stderr).expect("deadline stderr is UTF-8"),
        "conformance-regressions: timed out after 1s: Kotlin 2.4.10\n"
    );
    assert!(elapsed.as_secs() < 5, "deadline took {elapsed:?}");
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[test]
fn ci_runs_every_prebuilt_conformance_test_in_each_version_lane() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workflow =
        fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci workflow");
    let matrix = workflow
        .find("  conformance:\n")
        .expect("workflow has one supported-version conformance matrix");
    assert!(
        workflow.contains("version: ${{ fromJson(needs.versions.outputs.matrix) }}"),
        "conformance job must expand every supported Kotlin version"
    );
    let box_run = workflow[matrix..]
        .find("just conformance-run \"$PWD/conformance-bin\" \"${{ matrix.version }}\"")
        .map(|offset| matrix + offset)
        .expect("each version lane runs the box suite");
    let regressions = workflow[matrix..]
        .find("just conformance-regressions \"$PWD/conformance-bin\" \"${{ matrix.version }}\"")
        .map(|offset| matrix + offset)
        .expect("each version lane runs every non-box conformance test");
    let release = workflow
        .find(
            "needs: [ci, klib-semantics, conformance, build-gradle-plugin, gradle, versions, build-release]",
        )
        .expect(
            "release waits on combined conformance plus the exact tested Gradle artifact and matrix",
        );
    assert!(
        matrix < box_run && box_run < regressions && regressions < release,
        "one matrix lane runs box then non-box tests before release"
    );
    assert!(
        workflow.contains("target/cache/ser-corpus/${{ matrix.version }}"),
        "each version lane caches its matching serialization corpus"
    );
    assert!(
        workflow.contains("kotlin-conformance-all-${{ matrix.version }}-"),
        "the expanded corpus cache must not reuse the immutable box-only key"
    );
    assert!(
        workflow.contains("box-corpus-mock-jdk-${{ matrix.version }}-"),
        "the box corpus cache must not reuse an immutable key from before its mock JDK input"
    );
    assert!(
        workflow.contains("recorded-kotlinc-bytes-conformance-${{ matrix.version }}-"),
        "recorded kotlinc bytes must be isolated by matrix version"
    );
    assert!(
        !workflow.contains("\n  conformance-regressions:\n"),
        "non-box conformance must not remain a max-version-only job"
    );
    assert_eq!(
        workflow.matches("bin=$(just conformance-bin)").count(),
        1,
        "the matrix reuses one conformance build"
    );
    assert_eq!(
        workflow
            .matches("cargo build --profile gate -p krusty-cli")
            .count(),
        1,
        "the matrix reuses one CLI build"
    );
    assert_eq!(
        workflow.matches("bin=$(just krusty-build-tests)").count(),
        1,
        "Gradle lanes reuse one krusty-build test binary"
    );
    assert!(
        !workflow.contains("cargo test --profile gate -p krusty-build"),
        "a Gradle lane must not compile krusty"
    );
    assert!(
        workflow.contains("./krusty-build-tests --ignored"),
        "a Gradle lane runs the prebuilt krusty-build tests"
    );
    assert!(
        workflow.contains("needs: [build-shared-bins, versions]")
            && workflow.contains("needs: [build-shared-bins, build-gradle-plugin]"),
        "conformance and Gradle both wait on the shared binaries"
    );
    assert!(
        !workflow.contains("build-conformance-bin"),
        "the shared binary job is not named for conformance alone"
    );
    assert!(
        workflow.contains("phase_log_prefix=shared-bins")
            && workflow.contains("phase_begin conformance-test-binary")
            && workflow.contains("phase_begin krusty-cli")
            && workflow.contains("phase_begin krusty-build-tests"),
        "the shared binary job must time each compile"
    );
    assert!(
        workflow.contains(
            "\nconcurrency:\n  group: ci-${{ github.ref == 'refs/heads/master' && github.run_id || github.ref }}\n  cancel-in-progress: ${{ github.ref != 'refs/heads/master' }}\n"
        ),
        "PR and merge-group updates may auto-cancel, but each master build needs a unique group so it always finishes"
    );
    assert!(
        workflow.contains("KRUSTY_REQUIRE_KSP_E2E: \"1\"")
            && workflow.contains("KRUSTY_REQUIRE_SERIALIZATION_CONFORMANCE: \"1\""),
        "matrix conformance must fail instead of self-skipping required integrations"
    );
}

#[test]
fn overlapping_callers_hold_distinct_servers_up_to_the_cap() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::thread;
    use std::time::Duration;

    let pool = super::ServerPool::<usize>::new();
    let next_id = AtomicUsize::new(0);
    let active = AtomicUsize::new(0);
    let max_active = AtomicUsize::new(0);
    let seen = Mutex::new(Vec::new());
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered_tx, entered_rx) = mpsc::channel();

    thread::scope(|scope| {
        for _ in 0..4 {
            let pool = &pool;
            let next_id = &next_id;
            let active = &active;
            let max_active = &max_active;
            let seen = &seen;
            let entered_tx = entered_tx.clone();
            let release = Arc::clone(&release);
            scope.spawn(move || {
                pool.with_server(
                    2,
                    || Some(next_id.fetch_add(1, Ordering::Relaxed)),
                    |id| {
                        let now = active.fetch_add(1, Ordering::AcqRel) + 1;
                        max_active.fetch_max(now, Ordering::Relaxed);
                        seen.lock().unwrap_or_else(|err| err.into_inner()).push(*id);
                        entered_tx.send(*id).expect("controller receives admission");

                        let (released, available) = &*release;
                        let released = released.lock().unwrap_or_else(|err| err.into_inner());
                        let (released, _) = available
                            .wait_timeout_while(released, Duration::from_secs(5), |released| {
                                !*released
                            })
                            .unwrap_or_else(|err| err.into_inner());
                        assert!(
                            *released,
                            "controller did not release an admitted server within 5 seconds"
                        );
                        active.fetch_sub(1, Ordering::AcqRel);
                    },
                )
                .expect("cap allows a server");
            });
        }
        drop(entered_tx);

        let first = entered_rx.recv_timeout(Duration::from_secs(5));
        let second = entered_rx.recv_timeout(Duration::from_secs(5));
        {
            let (released, available) = &*release;
            *released.lock().unwrap_or_else(|err| err.into_inner()) = true;
            available.notify_all();
        }

        let first = first.expect("first caller did not enter within 5 seconds");
        let second = second.expect("second caller did not enter concurrently within 5 seconds");
        assert_ne!(
            first, second,
            "concurrent callers must hold distinct server claims"
        );
    });

    let seen = seen.lock().unwrap_or_else(|err| err.into_inner());
    assert_eq!(seen.len(), 4, "every caller must complete");
    let mut ids = seen.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids,
        vec![0, 1],
        "both servers must run, not a queue on the first"
    );
    assert_eq!(max_active.load(Ordering::Relaxed), 2);
    assert_eq!(next_id.load(Ordering::Relaxed), 2);
}

#[test]
fn a_finished_server_is_reused_instead_of_growing_the_pool() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = super::ServerPool::new();
    let next_id = AtomicUsize::new(0);
    let mut seen = Vec::new();
    for _ in 0..4 {
        pool.with_server(
            2,
            || Some(next_id.fetch_add(1, Ordering::Relaxed)),
            |id| seen.push(*id),
        )
        .expect("server");
    }
    assert_eq!(seen, vec![0, 0, 0, 0]);
    assert_eq!(next_id.load(Ordering::Relaxed), 1);
}

#[test]
fn a_panicking_caller_releases_its_server_claim() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = super::ServerPool::new();
    let next_id = AtomicUsize::new(0);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        pool.with_server(
            1,
            || Some(next_id.fetch_add(1, Ordering::Relaxed)),
            |_| panic!("test caller panic"),
        )
    }));
    assert!(panic.is_err());

    let reused = pool
        .with_server(
            1,
            || Some(next_id.fetch_add(1, Ordering::Relaxed)),
            |id| *id,
        )
        .expect("released server");
    assert_eq!(reused, 0);
    assert_eq!(next_id.load(Ordering::Relaxed), 1);
}

#[cfg(unix)]
#[test]
fn phase_timing_records_start_duration_and_summary() {
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("phase-timing.sh");
    let temp = std::env::temp_dir().join(format!("krusty-phase-timing-{}", std::process::id()));
    fs::create_dir_all(&temp).expect("create phase timing directory");
    let log = temp.join("phases.tsv");
    let output = Command::new("bash")
        .args([
            "-c",
            "source \"$1\"; phase_log_prefix=coverage; PHASE_TIMING_LOG=\"$2\"; phase_begin provision; phase_end provision; phase_begin build-cli; phase_end build-cli; phase_report",
            "phase-timing-test",
        ])
        .arg(&helper)
        .arg(&log)
        .output()
        .expect("run phase timing helper");
    assert!(
        output.status.success(),
        "phase timing failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "phase timing writes durations to stderr"
    );
    let stderr = String::from_utf8(output.stderr).expect("phase timing stderr is UTF-8");
    let lines = stderr.lines().collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            "coverage: phase start provision",
            "coverage: phase provision 0s",
            "coverage: phase start build-cli",
            "coverage: phase build-cli 0s",
            "coverage: phases",
            "coverage: phase provision 0s",
            "coverage: phase build-cli 0s",
            "coverage: phase total 0s",
        ],
        "phase timing lines: {stderr}"
    );
    fs::remove_dir_all(temp).expect("remove phase timing directory");
}

#[cfg(unix)]
#[test]
fn phase_timing_coverage_run_prints_every_phase() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = std::env::temp_dir().join(format!("krusty-coverage-phases-{}", std::process::id()));
    fs::create_dir_all(&temp).expect("create coverage phase directory");
    let bin_dir = temp.join("bin");
    fs::create_dir_all(&bin_dir).expect("create stub directory");
    let e2e_bin = temp.join("e2e");
    let unit_bin = temp.join("lsp-unit");
    let target = temp.join("target");
    let summary = temp.join("summary.json");
    let compiler_json = temp.join("compiler.json");
    let lsp_json = temp.join("lsp.json");
    fs::write(
        &e2e_bin,
        "#!/usr/bin/env bash\nif [ \"${1:-}\" = --list ]; then printf '%s\\n' 'alpha::one: test' '' '1 test, 0 benchmarks'; exit 0; fi\nprintf '%s\\n' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\n",
    )
    .expect("write e2e stub");
    fs::write(
        &unit_bin,
        "#!/usr/bin/env bash\nprintf '%s\\n' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\n",
    )
    .expect("write unit stub");
    fs::write(bin_dir.join("just"), "#!/bin/sh\nexit 0\n").expect("write just stub");
    fs::write(
        bin_dir.join("nproc"),
        "#!/bin/sh\n[ \"${STUB_CPU_SOURCE:-}\" = nproc ] || exit 1\nprintf '%s\\n' 7\n",
    )
    .expect("write nproc stub");
    fs::write(
        bin_dir.join("sysctl"),
        "#!/bin/sh\n[ \"${STUB_CPU_SOURCE:-}\" = sysctl ] || exit 1\n[ \"$*\" = '-n hw.ncpu' ] || exit 2\nprintf '%s\\n' 5\n",
    )
    .expect("write sysctl stub");
    let build_log = temp.join("builds.txt");
    fs::write(
        bin_dir.join("cargo"),
        "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \"${1:-}\" == +* ]]; then shift; fi\ncmd=\"${1:-}\"; shift || true\ncase \"$cmd\" in\n  llvm-cov)\n    sub=\"${1:-}\"; shift || true\n    case \"$sub\" in\n      --version) exit 0 ;;\n      show-env) printf '%s\\n' true ;;\n      report)\n        out=\"\"\n        while [ $# -gt 0 ]; do\n          if [ \"$1\" = --output-path ]; then out=\"$2\"; shift 2; else shift; fi\n        done\n        mkdir -p \"$(dirname \"$out\")\"\n        printf '%s\\n' '{\"data\":[{\"totals\":{\"regions\":{\"covered\":1,\"count\":2},\"functions\":{\"covered\":1,\"count\":2},\"lines\":{\"covered\":1,\"count\":2},\"branches\":{\"covered\":1,\"count\":2}}}]}' >\"$out\"\n        ;;\n      *) echo \"unexpected llvm-cov $sub\" >&2; exit 2 ;;\n    esac\n    ;;\n  build)\n    printf '%s\\n' \"$*\" >>\"$STUB_BUILD_LOG\"\n    echo \"cargo-rustflags:${RUSTFLAGS-}\" >&2\n    case \" $* \" in\n      *\" -p krusty-cli \"*) ;;\n      *) echo \"unexpected build $*\" >&2; exit 2 ;;\n    esac\n    case \" $* \" in\n      *\" -p krusty-lsp \"*) ;;\n      *) echo \"unexpected build $*\" >&2; exit 2 ;;\n    esac\n    case \" $* \" in\n      *\" --bin krusty \"*) ;;\n      *) echo \"unexpected build $*\" >&2; exit 2 ;;\n    esac\n    case \" $* \" in\n      *\" --bin krusty-lsp \"*) ;;\n      *) echo \"unexpected build $*\" >&2; exit 2 ;;\n    esac\n    mkdir -p \"$CARGO_TARGET_DIR/coverage\"\n    printf '%s\\n' '#!/bin/sh' 'exit 0' >\"$CARGO_TARGET_DIR/coverage/krusty\"\n    printf '%s\\n' '#!/bin/sh' 'exit 0' >\"$CARGO_TARGET_DIR/coverage/krusty-lsp\"\n    chmod +x \"$CARGO_TARGET_DIR/coverage/krusty\" \"$CARGO_TARGET_DIR/coverage/krusty-lsp\"\n    ;;\n  test)\n    echo \"cargo: visible stderr $*\" >&2\n    if [[ \" $* \" == *\" --test e2e \"* ]]; then\n      printf '%s\\n' \"{\\\"profile\\\":{\\\"test\\\":true},\\\"executable\\\":\\\"$E2E_BIN\\\"}\"\n    else\n      printf '%s\\n' \"{\\\"profile\\\":{\\\"test\\\":true},\\\"executable\\\":\\\"$UNIT_BIN\\\"}\"\n    fi\n    ;;\n  *) echo \"unexpected cargo $cmd $*\" >&2; exit 2 ;;\nesac\n",
    )
    .expect("write cargo stub");
    for name in ["e2e", "lsp-unit", "just", "cargo", "nproc", "sysctl"] {
        let path = if matches!(name, "just" | "cargo" | "nproc" | "sysctl") {
            bin_dir.join(name)
        } else if name == "e2e" {
            e2e_bin.clone()
        } else {
            unit_bin.clone()
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("make stub executable");
    }

    let run_coverage = |cpu_source: &str| {
        Command::new("bash")
            .arg(root.join("scripts").join("coverage.sh"))
            .arg(&summary)
            .env("HOME", &temp)
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .env("CARGO_TARGET_DIR", &target)
            .env("KRUSTY_COVERAGE_TARGET_DIR", &target)
            .env("KRUSTY_COVERAGE_COMPILER_JSON", &compiler_json)
            .env("KRUSTY_COVERAGE_LSP_JSON", &lsp_json)
            .env("KRUSTY_COVERAGE_E2E_SHARDS", "1")
            .env("KRUSTY_TEST_JOBS", "1")
            .env("KRUSTY_TEST_THREADS", "1")
            .env("RUSTFLAGS", "-Z threads=4")
            .env("E2E_BIN", &e2e_bin)
            .env("UNIT_BIN", &unit_bin)
            .env("STUB_BUILD_LOG", &build_log)
            .env("STUB_CPU_SOURCE", cpu_source)
            .output()
            .expect("run coverage with stubbed toolchain")
    };
    let output = run_coverage("nproc");
    let stderr = String::from_utf8(output.stderr).expect("coverage stderr is UTF-8");
    assert!(
        output.status.success(),
        "coverage stub run failed: {stderr}"
    );
    assert!(
        stderr.contains("cargo: visible stderr"),
        "compiler test build hid cargo stderr: {stderr}"
    );
    let builds = fs::read_to_string(&build_log).unwrap_or_default();
    assert_eq!(
        builds.lines().count(),
        1,
        "CLI and language server must be one cargo build: {builds}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter_map(|line| line.strip_prefix("cargo-rustflags:"))
            .collect::<Vec<_>>(),
        ["-Z threads=4 -Z threads=7"],
        "nproc frontend threads were not appended: {stderr}"
    );
    fs::write(&build_log, "").expect("reset build log");
    let fallback_output = run_coverage("sysctl");
    let fallback_stderr =
        String::from_utf8(fallback_output.stderr).expect("fallback coverage stderr is UTF-8");
    assert!(
        fallback_output.status.success(),
        "coverage sysctl fallback run failed: {fallback_stderr}"
    );
    assert_eq!(
        fallback_stderr
            .lines()
            .filter_map(|line| line.strip_prefix("cargo-rustflags:"))
            .collect::<Vec<_>>(),
        ["-Z threads=4 -Z threads=5"],
        "sysctl frontend threads were not appended: {fallback_stderr}"
    );
    let starts = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("coverage: phase start "))
        .collect::<Vec<_>>();
    assert_eq!(
        starts,
        [
            "provision",
            "instrument",
            "build-bins",
            "build-compiler-tests",
            "build-lsp-tests",
            "test-lsp-unit",
            "plan-e2e-shards",
            "e2e-shard-1-of-1",
            "report-compiler",
            "report-lsp",
        ],
        "phase starts: {stderr}"
    );
    for name in starts {
        assert!(
            stderr.contains(&format!("coverage: phase {name} "))
                && stderr
                    .lines()
                    .any(|line| line.starts_with(&format!("coverage: phase {name} "))
                        && line.rsplit_once(' ').is_some_and(|(_, seconds)| {
                            seconds.ends_with('s')
                                && seconds[..seconds.len() - 1]
                                    .chars()
                                    .all(|ch| ch.is_ascii_digit())
                        })),
            "missing duration for {name}: {stderr}"
        );
    }
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("coverage: phase total ")
                && line
                    .rsplit_once(' ')
                    .is_some_and(|(_, seconds)| seconds.ends_with('s'))),
        "missing phase total: {stderr}"
    );
    assert!(summary.is_file(), "coverage summary was not written");
    fs::remove_dir_all(temp).expect("remove coverage phase directory");
}
