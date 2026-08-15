use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
            "unset KRUSTY_TEST_TIMEOUT_SECONDS KRUSTY_CONFORMANCE_TIMEOUT_SECONDS KRUSTY_E2E_TIMEOUT_SECONDS KRUSTY_E2E_SHARDS; source \"$1\"; printf '%s\\n' \"$KRUSTY_TEST_TIMEOUT_SECONDS\" \"$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS\" \"$KRUSTY_E2E_TIMEOUT_SECONDS\" \"$KRUSTY_E2E_SHARDS\"",
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
    assert_eq!(values, [120, 295, 295, 22]);
    assert!(values[..3].iter().all(|seconds| *seconds < 300));
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
