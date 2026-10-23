use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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

#[test]
fn canonical_gate_defaults_bound_ordinary_and_single_process_suites() {
    let defaults = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("test-gate-defaults.sh");
    let output = Command::new("bash")
        .args([
            "-c",
            "unset KRUSTY_TEST_TIMEOUT_SECONDS KRUSTY_CONFORMANCE_TIMEOUT_SECONDS KRUSTY_E2E_TIMEOUT_SECONDS KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS KRUSTY_CONFORMANCE_SHARDS KRUSTY_SCORED_CONFORMANCE_SHARDS KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS; source \"$1\"; if [ -n \"${KRUSTY_CONFORMANCE_SHARDS+set}\" ] || [ -n \"${KRUSTY_SCORED_CONFORMANCE_SHARDS+set}\" ] || [ -n \"${KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS+set}\" ]; then echo 'conformance shard settings must stay retired' >&2; exit 1; fi; printf '%s\\n' \"$KRUSTY_TEST_TIMEOUT_SECONDS\" \"$KRUSTY_CONFORMANCE_TIMEOUT_SECONDS\" \"$KRUSTY_E2E_TIMEOUT_SECONDS\" \"$KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS\" \"$KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS\" \"$KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS\"",
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
    assert_eq!(values, [120, 120, 1800, 600, 600, 1800]);
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
        // The retired scored-only deadline must not lengthen the canonical one.
        .env("KRUSTY_SCORED_CONFORMANCE_TIMEOUT_SECONDS", "300")
        .output()
        .expect("run delayed conformance fixture");
    let elapsed = started.elapsed();

    assert_eq!(output.status.code(), Some(124));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        without_phase_timing(&String::from_utf8(output.stderr).expect("deadline stderr is UTF-8")),
        "conformance-run: timed out after 1s: Kotlin 2.4.10\n"
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
        "#!/usr/bin/env bash\nprintf '%s|%s|%s|%s|%s\\n' \"$KRUSTY_LANGUAGE_VERSION\" \"$KRUSTY_KOTLINC\" \"$KRUSTY_KOTLIN_BOX_DIR\" \"$1\" \"$2\" >&2\nprintf '62.5 5 8\\n' >\"$KRUSTY_CONFORMANCE_REPORT\"\nprintf '25.0 1 4\\n' >\"$KRUSTY_JVM_BYTE_REPORT\"\n",
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
    assert_eq!(output.stdout, b"62.5 5 8\n");
    let stderr = String::from_utf8(output.stderr).expect("report stderr is UTF-8");
    assert_eq!(
        without_phase_timing(&stderr),
        format!(
            "2.4.10|/bin/reference-kotlinc|{}|kotlin_codegen_box_conformance|--nocapture\n\
             conformance-run: Kotlin 2.4.10 JVM byte equality (matched/total .class bytes): 25.0 1 4\n",
            temp.display(),
        )
    );
    fs::remove_dir_all(temp).expect("remove conformance report test directory");
}

#[cfg(unix)]
#[test]
fn prebuilt_conformance_runner_preserves_reports_from_a_failing_process() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = root.join("scripts").join("conformance-run.sh");
    let temp = std::env::temp_dir().join(format!(
        "krusty-conformance-run-failing-process-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create failing-process test directory");
    let binary = temp.join("conformance-bin");
    fs::write(
        &binary,
        "#!/usr/bin/env bash\nprintf 'fixture failed after writing reports\\n' >&2\nprintf '50.0 1 2\\n' >\"$KRUSTY_CONFORMANCE_REPORT\"\nprintf '10.0 1 10\\n' >\"$KRUSTY_JVM_BYTE_REPORT\"\nexit 101\n",
    )
    .expect("write failing-process conformance fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
        .expect("make failing-process conformance fixture executable");

    let output = Command::new("bash")
        .arg(runner)
        .arg(&binary)
        .arg("2.4.10")
        .env("KRUSTY_KOTLINC", "/bin/reference-kotlinc")
        .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
        .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .output()
        .expect("run failing-process conformance fixture");

    assert_eq!(output.status.code(), Some(101));
    assert_eq!(output.stdout, b"50.0 1 2\n");
    let stderr = String::from_utf8(output.stderr).expect("failing-process stderr is UTF-8");
    assert_eq!(
        without_phase_timing(&stderr),
        "fixture failed after writing reports\n\
         conformance-run: Kotlin 2.4.10 JVM byte equality (matched/total .class bytes): 10.0 1 10\n"
    );
    fs::remove_dir_all(temp).expect("remove failing-process test directory");
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

#[test]
fn full_local_conformance_uses_target_owned_focused_passes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runner = fs::read_to_string(root.join("run-tests.sh")).expect("read run-tests.sh");
    for required in [
        "run-tests.sh: conformance native-focused",
        "KRUSTY_REQUIRE_NATIVE_CONFORMANCE=1",
        "kotlin_box_native_conformance:: --skip kotlin_box_native_conformance::kotlin_codegen_box_native_conformance",
        "run-tests.sh: conformance native-multimodule",
        "KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER=native",
        "run-tests.sh: conformance wasm-focused",
        "KRUSTY_REQUIRE_WASM_JS_CONFORMANCE=1",
        "KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE=1",
        "kotlin_box_wasm_conformance:: --skip kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_js_conformance --skip kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_wasi_conformance",
        "run-tests.sh: conformance wasm-multimodule",
        "KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER=wasm",
        "multimodule_conformance_cases:: --test-threads=1",
        "--skip kotlin_codegen_box_conformance --skip kotlin_box_native_conformance:: --skip kotlin_box_wasm_conformance::",
    ] {
        assert!(
            runner.contains(required),
            "the canonical full gate must preserve target ownership ({required})"
        );
    }

    let justfile = fs::read_to_string(root.join("justfile")).expect("read justfile");
    for recipe in ["conformance-all-plain", "conformance-one"] {
        let body = justfile
            .split(&format!("\n{recipe}"))
            .nth(1)
            .and_then(|text| text.split("\n\n").next())
            .unwrap_or_else(|| panic!("justfile has {recipe}"));
        for required in [
            "target-conformance-regressions.sh native",
            "target-conformance-regressions.sh wasm",
            "KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER=jvm",
            "--skip kotlin_box_native_conformance::",
            "--skip kotlin_box_wasm_conformance::",
        ] {
            assert!(
                body.contains(required)
                    || (recipe == "conformance-all-plain"
                        && required == "target-conformance-regressions.sh native"
                        && body.contains("native-conformance-regressions"))
                    || (recipe == "conformance-all-plain"
                        && required == "target-conformance-regressions.sh wasm"
                        && body.contains("wasm-conformance-regressions")),
                "{recipe} must preserve target ownership ({required})"
            );
        }
    }
}

#[test]
fn target_focused_modules_do_not_consume_jvm_oracles() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for module in [
        "tests/kotlin_box_native_conformance.rs",
        "tests/kotlin_box_wasm_conformance.rs",
    ] {
        let source = fs::read_to_string(root.join(module)).expect("read target conformance module");
        for forbidden in [
            "reference_jvm_acceptance",
            "expect_box_ok_with_stdlib",
            "stdlib_jar",
        ] {
            assert!(
                !source.contains(forbidden),
                "{module} must assert real target KLIB/runtime outcomes, not {forbidden}"
            );
        }
    }
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
fn native_runner_rejects_a_missing_report_and_truncates_the_callers_copy() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture("native-missing-report", "#!/bin/sh\nexit 0\n");
    let report = temp.join("native-pct.txt");
    fs::write(&report, "100.0 9 9\n").expect("write stale Native report");

    let output = Command::new("bash")
        .arg(root.join("scripts").join("native-conformance-run.sh"))
        .arg(&binary)
        .arg(&report)
        .env("KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .output()
        .expect("run Native conformance fixture without a report");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read(&report).expect("read truncated report"), b"");
    assert_eq!(
        String::from_utf8(output.stderr).expect("native stderr is UTF-8"),
        "native-conformance-run: test did not write a valid \"<pct> <passed> <applicable>\" report\n"
    );
    fs::remove_dir_all(temp).expect("remove Native missing-report fixture");
}

#[cfg(unix)]
#[test]
fn prebuilt_jvm_regressions_skip_the_runnable_target_modules() {
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
            "bin={}\n--skip\nkotlin_codegen_box_conformance\n--skip\nkotlin_box_native_conformance::\n--skip\nkotlin_box_wasm_conformance::\n--test-threads\n{}\n\
             bin={}\n--ignored\n--exact\n\
             kotlin_cli_jvm_conformance::the_reference_compiler_reproduces_every_applicable_cli_case\n",
            sibling.display(),
            regression_threads(),
            sibling.display(),
        ),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[cfg(unix)]
#[test]
fn the_cli_reference_run_compiles_missing_recordings_unless_told_otherwise() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture(
        "cli-reference-recordings",
        "#!/usr/bin/env bash\nprintf '%s compile_missing=%s\\n' \"$1\" \
         \"${KRUSTY_CLASS_DUMP_COMPILE_MISSING:-unset}\"\n",
    );
    let run = |compile_missing: Option<&str>| {
        let mut command = Command::new("bash");
        command
            .arg(root.join("scripts").join("conformance-regressions.sh"))
            .arg(&binary)
            .arg("2.4.10")
            .env("KRUSTY_KOTLINC", "/bin/true")
            .env("KRUSTY_KOTLIN_BOX_DIR", &temp)
            .env("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS", "3")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", temp.join("bin").display()),
            );
        match compile_missing {
            Some(value) => command.env("KRUSTY_CLASS_DUMP_COMPILE_MISSING", value),
            None => command.env_remove("KRUSTY_CLASS_DUMP_COMPILE_MISSING"),
        };
        let output = command.output().expect("run conformance regressions");
        assert_eq!(
            output.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("regressions stdout is UTF-8")
    };

    assert_eq!(
        run(None),
        "--skip compile_missing=unset\n--ignored compile_missing=1\n"
    );
    assert_eq!(
        run(Some("0")),
        "--skip compile_missing=0\n--ignored compile_missing=0\n"
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
            "bin={}\n--skip\nkotlin_codegen_box_conformance\n--skip\nkotlin_box_native_conformance::\n--skip\nkotlin_box_wasm_conformance::\n--test-threads\n{}\n\
             bin={}\n--ignored\n--exact\n\
             kotlin_cli_jvm_conformance::the_reference_compiler_reproduces_every_applicable_cli_case\n",
            sibling.display(),
            regression_threads(),
            sibling.display(),
        ),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(temp).expect("remove regressions fixture");
}

#[cfg(unix)]
#[test]
fn target_regressions_run_their_focused_module_and_shared_cases_after_provisioning() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (temp, binary) = regression_fixture(
        "target-ownership",
        "#!/usr/bin/env bash\nprintf 'language=%s native=%s js=%s wasi=%s runner=%s\\n' \
         \"$KRUSTY_LANGUAGE_VERSION\" \"${KRUSTY_REQUIRE_NATIVE_CONFORMANCE:-0}\" \
         \"${KRUSTY_REQUIRE_WASM_JS_CONFORMANCE:-0}\" \
         \"${KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE:-0}\" \
         \"${KRUSTY_MULTIMODULE_CONFORMANCE_RUNNER:-unset}\"\nprintf '%s\\n' \"$@\"\n",
    );
    let node = temp.join("bin").join("node");
    fs::write(&node, "#!/bin/sh\nexit 0\n").expect("write node stub");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).expect("make node executable");
    let path = format!("{}:/usr/bin:/bin", temp.join("bin").display());

    let native = Command::new("bash")
        .arg(root.join("scripts/target-conformance-regressions.sh"))
        .args(["native", binary.to_str().unwrap(), "2.4.10"])
        .env("KRUSTY_KOTLIN_NATIVE", &temp)
        .env_remove("KRUSTY_KOTLINC")
        .env_remove("KRUSTY_KOTLIN_BOX_DIR")
        .env("KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .env_remove("KRUSTY_REQUIRE_NATIVE_CONFORMANCE")
        .env_remove("KRUSTY_REQUIRE_WASM_JS_CONFORMANCE")
        .env_remove("KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE")
        .env("PATH", &path)
        .output()
        .expect("run Native target regressions");
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(native.stdout).expect("Native stdout is UTF-8"),
        "language=2.4.10 native=1 js=0 wasi=0 runner=native\n\
         kotlin_box_native_conformance::\n--skip\n\
         kotlin_box_native_conformance::kotlin_codegen_box_native_conformance\n\
         --test-threads\n1\n\
         language=2.4.10 native=1 js=0 wasi=0 runner=native\n\
         multimodule_conformance_cases::\n--test-threads\n1\n"
    );

    let wasm = Command::new("bash")
        .arg(root.join("scripts/target-conformance-regressions.sh"))
        .args(["wasm", binary.to_str().unwrap(), "2.4.10"])
        .env("KRUSTY_KOTLINC", "/bin/true")
        .env("KRUSTY_DEPS_CACHE", &temp)
        .env_remove("KRUSTY_KOTLIN_BOX_DIR")
        .env("KRUSTY_TARGET_CONFORMANCE_TIMEOUT_SECONDS", "3")
        .env_remove("KRUSTY_REQUIRE_NATIVE_CONFORMANCE")
        .env_remove("KRUSTY_REQUIRE_WASM_JS_CONFORMANCE")
        .env_remove("KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE")
        .env("PATH", &path)
        .output()
        .expect("run Wasm target regressions");
    assert_eq!(wasm.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(wasm.stdout).expect("Wasm stdout is UTF-8"),
        "language=2.4.10 native=0 js=1 wasi=1 runner=wasm\n\
         kotlin_box_wasm_conformance::\n--skip\n\
         kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_js_conformance\n--skip\n\
         kotlin_box_wasm_conformance::kotlin_codegen_box_wasm_wasi_conformance\n\
         --test-threads\n1\n\
         language=2.4.10 native=0 js=1 wasi=1 runner=wasm\n\
         multimodule_conformance_cases::\n--test-threads\n1\n"
    );
    fs::remove_dir_all(temp).expect("remove target ownership fixture");
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
    assert_eq!(output.stdout, b"bin=/opt/krusty\nbin=/opt/krusty\n");
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
fn ci_starts_jvm_without_waiting_for_focused_klib_binaries() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workflow =
        fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci workflow");
    let matrix = workflow
        .find("  conformance:\n")
        .expect("workflow has one supported-version conformance matrix");
    assert!(
        workflow.contains("version: ${{ fromJson(needs.build-tool-bins.outputs.matrix) }}"),
        "conformance job must expand every supported Kotlin version"
    );
    assert!(
        workflow
            .matches("name: conformance (${{ matrix.version }}, ${{ matrix.target }})")
            .count()
            == 2
            && workflow.matches("        target: [jvm]\n").count() == 1
            && workflow.matches("        target: [native, wasm]\n").count() == 1
            && workflow.matches("steps: &conformance-steps").count() == 1
            && workflow.matches("steps: *conformance-steps").count() == 1,
        "each exact Kotlin version must expose JVM, Native and Wasm rows from one shared inventory"
    );
    let box_run = workflow[matrix..]
        .find("just conformance-run \"$PWD/conformance-bin\" \"${{ matrix.version }}\"")
        .map(|offset| matrix + offset)
        .expect("each version's JVM row runs the box suite");
    let native_box_run = workflow[matrix..]
        .find("just native-conformance-run \"$PWD/conformance-bin\" \"${{ matrix.version }}\"")
        .map(|offset| matrix + offset)
        .expect("each version's Native row runs the native box suite");
    let native_regressions = workflow[matrix..]
        .find(
            "just native-conformance-regressions \"$PWD/conformance-bin\" \"${{ matrix.version }}\"",
        )
        .map(|offset| matrix + offset)
        .expect("each version's Native row runs its focused target tests");
    let wasm_regressions = workflow[matrix..]
        .find(
            "just wasm-conformance-regressions \"$PWD/conformance-bin\" \"${{ matrix.version }}\"",
        )
        .map(|offset| matrix + offset)
        .expect("each version's Wasm row runs its focused target tests");
    let regressions = workflow[matrix..]
        .find("just conformance-regressions \"$PWD/conformance-bin\" \"${{ matrix.version }}\"")
        .map(|offset| matrix + offset)
        .expect("each version's JVM row runs every non-box conformance test");
    let release = workflow
        .find(
            "needs: [ci, lint, conformance, conformance-klib, build-gradle-plugin, gradle, build-maven-plugin, maven, build-tools, bazel, build-tool-bins, build-release]",
        )
        .expect(
            "release waits on combined conformance plus the exact tested Gradle and Maven artifacts and matrices",
        );
    assert!(
        matrix < box_run
            && box_run < native_box_run
            && native_box_run < native_regressions
            && native_regressions < wasm_regressions
            && wasm_regressions < regressions
            && regressions < release
    );
    for needle in [
        "- name: run box conformance ${{ matrix.version }}\n        if: matrix.target == 'jvm'",
        "- name: run native box conformance ${{ matrix.version }}\n        if: matrix.target == 'native'",
        "- name: run wasm box conformance ${{ matrix.version }}\n        if: matrix.target == 'wasm'",
        "- name: run focused Native conformance ${{ matrix.version }}\n        # Keep focused target regressions independent",
        "- name: run focused Wasm conformance ${{ matrix.version }}\n        if: matrix.target == 'wasm'",
        "- name: run non-box conformance ${{ matrix.version }}\n        if: matrix.target == 'jvm'",
    ] {
        assert!(
            workflow.contains(needle),
            "target ownership must be explicit in the matrix ({needle})"
        );
    }
    assert!(
        workflow.contains("target/cache/ser-corpus/${{ matrix.version }}"),
        "each version lane caches its matching serialization corpus"
    );
    assert!(
        workflow.contains("kotlin-conformance-all-${{ matrix.version }}-"),
        "the expanded corpus cache must not reuse the immutable box-only key"
    );
    assert!(
        workflow.contains("kotlin-${{ matrix.target }}-conformance-${{ matrix.version }}-")
            && workflow.contains("if: matrix.target != 'jvm'"),
        "a Native or Wasm row must not race to publish the JVM row's broader immutable cache"
    );
    for key in [
        "key: box-and-cli-corpus-v1-${{ hashFiles('kotlin-versions') }}",
        "key: box-and-cli-corpus-v1-${{ matrix.version }}-${{ hashFiles('kotlin-versions') }}",
    ] {
        assert!(
            workflow.contains(key),
            "every box corpus cache must use the key versioned for its mock JDK and CLI corpus inputs ({key})"
        );
    }
    assert!(
        !workflow.contains("box-corpus-mock-jdk-"),
        "no box corpus cache may restore an immutable entry from before the CLI corpus input"
    );
    assert_eq!(
        workflow.matches("path: target/cache/box-corpus").count(),
        2,
        "only the coverage and conformance jobs read the box and CLI corpus"
    );
    let justfile = fs::read_to_string(root.join("justfile")).expect("read justfile");
    let klib_recipe = justfile
        .split("\nklib-semantics ")
        .nth(1)
        .and_then(|recipe| recipe.split("\n\n").next())
        .expect("justfile has a klib-semantics recipe");
    assert!(
        klib_recipe.contains("KRUSTY_PROVISION_BOX_CORPUS=0 "),
        "the focused local KLIB recipe must not provision an unrelated corpus"
    );
    let native_target_recipe = justfile
        .split("\nnative-conformance-regressions ")
        .nth(1)
        .and_then(|recipe| recipe.split("\n\n").next())
        .expect("justfile has a Native target regression recipe");
    for required in [
        "just kotlin-native",
        "target-conformance-regressions.sh native",
    ] {
        assert!(
            native_target_recipe.contains(required),
            "Native focused regressions must provision their exact target setup ({required})"
        );
    }
    let wasm_target_recipe = justfile
        .split("\nwasm-conformance-regressions ")
        .nth(1)
        .and_then(|recipe| recipe.split("\n\n").next())
        .expect("justfile has a Wasm target regression recipe");
    for required in ["just kotlinc", "target-conformance-regressions.sh wasm"] {
        assert!(
            wasm_target_recipe.contains(required),
            "Wasm focused regressions must provision their exact target setup ({required})"
        );
    }
    assert!(
        !native_target_recipe.contains("box-corpus")
            && !native_target_recipe.contains("KRUSTY_KOTLIN_BOX_DIR")
            && !native_target_recipe.contains("just kotlinc")
            && !native_target_recipe.contains("KRUSTY_KOTLINC")
            && !wasm_target_recipe.contains("box-corpus")
            && !wasm_target_recipe.contains("KRUSTY_KOTLIN_BOX_DIR"),
        "focused target regressions must not provision the unrelated box corpus"
    );
    assert_eq!(
        workflow
            .matches("if: steps.box-corpus.outputs.cache-hit == 'true'\n        run: echo KRUSTY_BOX_CORPUS_OFFLINE=1 >> \"$GITHUB_ENV\"")
            .count(),
        2,
        "a restored box corpus must be complete, so every lane forbids fetching after a hit"
    );
    assert!(
        workflow.contains("recorded-kotlinc-bytes-conformance-${{ matrix.version }}-"),
        "recorded kotlinc bytes must be isolated by matrix version"
    );
    let box_reference_producer_hash = "${{ hashFiles('tests/box_reference_classes.rs', 'tests/common/kotlinc_server.rs', 'tests/common/server_pool.rs', 'tests/common/mod.rs', 'tests/common/producing_jdk.rs', 'tests/common/byte_dump.rs', 'tests/common/byte_dump/**', 'src/conformance.rs', 'src/conformance/**') }}";
    let restore_reference_key = [
        "          path: target/cache/ref-classes\n          key: box-reference-classes-${{ matrix.version }}-",
        box_reference_producer_hash,
        "-${{ github.event_name == 'pull_request' && github.event.pull_request.base.sha",
    ]
    .concat();
    let save_reference_key = [
        "          path: target/cache/ref-classes\n          key: box-reference-classes-${{ matrix.version }}-",
        box_reference_producer_hash,
        "-${{ github.sha }}\n",
    ]
    .concat();
    for needle in [
        "      - name: restore kotlinc box references (${{ matrix.version }})\n        if: matrix.target == 'jvm'\n",
        restore_reference_key.as_str(),
        "      - name: save kotlinc box references (${{ matrix.version }})\n        if: matrix.target == 'jvm' && github.event_name == 'push' && github.ref == 'refs/heads/master' && steps.box-references.outputs.cache-hit != 'true'\n",
        save_reference_key.as_str(),
    ] {
        assert!(
            workflow.contains(needle),
            "each JVM row replays its version's kotlinc box references, and only master saves them ({needle})"
        );
    }
    let box_step = &workflow[workflow
        .find("      - name: run box conformance ${{ matrix.version }}\n")
        .expect("workflow has a JVM box conformance step")
        ..native_box_run];
    for needle in [
        "KRUSTY_CONFORMANCE_TIMEOUT_SECONDS: \"1800\"",
        "KRUSTY_CLASS_DUMP_DIR: ${{ github.workspace }}/target/cache/class-dumps",
        "KRUSTY_CLASS_DUMP_WRITE: ${{ steps.recorded-mode.outputs.write }}",
        "KRUSTY_CLASS_DUMP_COMPILE_MISSING: ${{ steps.recorded-mode.outputs.compile_missing }}",
    ] {
        assert!(
            box_step.contains(needle),
            "a cold JVM box lane must have time to populate its selected byte cache ({needle})"
        );
    }
    assert!(
        !workflow.contains("recorded-native-outcomes-")
            && !workflow.contains("KRUSTY_NATIVE_OUTCOME_CACHE"),
        "Native expectations are committed text, not an opaque GHA cache"
    );
    for directory in [
        "tests/box_expected_failures/native-klib",
        "tests/box_expected_not_applicable/native-klib",
    ] {
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(directory)
                .is_dir(),
            "Native expectations must be committed under the shared platform/version layout: {directory}"
        );
    }
    assert!(
        !workflow.contains("\n  conformance-regressions:\n"),
        "non-box conformance must not remain a max-version-only job"
    );
    let conformance_builds = fs::read_dir(root.join(".github/workflows"))
        .expect("read workflow directory")
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .map(|workflow| workflow.matches("bin=$(just conformance-bin)").count())
        .sum::<usize>();
    assert_eq!(
        conformance_builds, 1,
        "all GHA matrices must reuse one conformance build; ratchet updates are one-off local operations"
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
        workflow.contains("needs: [build-conformance-bin, build-tool-bins]")
            && workflow.contains(
                "needs: [build-conformance-bin, build-klib-test-bins, build-tool-bins]"
            )
            && workflow.contains("needs: [build-tool-bins, build-gradle-plugin]")
            && workflow.contains("needs: [build-tool-bins]"),
        "JVM starts before focused KLIB binaries while each other consumer waits only on its artifact families"
    );
    assert!(
        !workflow.contains("build-shared-bins"),
        "unrelated artifact families must not be serialized on one runner"
    );
    assert!(
        workflow.contains("phase_log_prefix=conformance-bin")
            && workflow.contains("phase_begin conformance-test-binary")
            && workflow.contains("phase_log_prefix=klib-test-bins")
            && workflow.contains("phase_begin focused-klib-test-binaries")
            && workflow.contains("scripts/build-klib-test-binaries.sh \"$PWD\"")
            && workflow.contains("phase_log_prefix=tool-bins")
            && workflow.contains("phase_begin krusty-cli")
            && workflow.contains("phase_begin krusty-build-tests"),
        "each independently scheduled build retains measurable phase ownership"
    );
    assert!(
        workflow.contains(
            "\nconcurrency:\n  group: ci-${{ github.ref == 'refs/heads/master' && github.run_id || github.ref }}\n  cancel-in-progress: ${{ github.ref != 'refs/heads/master' }}\n"
        ),
        "PR and merge-group updates may auto-cancel, but each master build needs a unique group so it always finishes"
    );
    assert!(
        workflow.contains("KRUSTY_REQUIRE_SERIALIZATION_CONFORMANCE: \"1\""),
        "matrix conformance must fail instead of self-skipping required serialization"
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

fn slow_test_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("libtest-shards.sh")
}

fn run_slow_tests(log: &Path, threshold: &str) -> std::process::Output {
    Command::new("bash")
        .args([
            "-c",
            "source \"$1\"; libtest_slow_tests \"$2\" \"$3\"",
            "slow-tests",
        ])
        .arg(slow_test_script())
        .arg(log)
        .arg(threshold)
        .output()
        .expect("list slow tests")
}

#[test]
fn slow_tests_are_those_over_the_threshold() {
    let dir = std::env::temp_dir().join(format!("krusty-slow-tests-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("slow-test fixture dir");
    let log = dir.join("sample.log");
    fs::write(
        &log,
        "\
running 5 tests
test fast::one ... ok <0.001s>
test boundary::exact ... ok <0.200s>
test boundary::over ... ok <0.201s>
test slow::fails ... FAILED <1.500s>
test slow::skipped ... ignored
test result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 1.70s
",
    )
    .expect("write sample log");

    let listed = run_slow_tests(&log, "200");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert_eq!(
        String::from_utf8(listed.stdout).expect("utf-8"),
        "1500\tslow::fails\n201\tboundary::over\n"
    );

    let shard = dir.join("shard-1-of-2.log");
    fs::write(&shard, "test other::case ... ok <0.250s>\n").expect("write shard log");
    let printed = Command::new("bash")
        .args([
            "-c",
            "source \"$1\"; KRUSTY_SLOW_TEST_MS=200 libtest_print_slow_tests \"$2\" \"$3\"",
            "print-slow-tests",
        ])
        .arg(slow_test_script())
        .arg(&log)
        .arg(&shard)
        .output()
        .expect("print slow tests");
    assert!(
        printed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&printed.stderr)
    );
    assert_eq!(
        String::from_utf8(printed.stdout).expect("utf-8"),
        "\
slow-test: summary count=3 threshold=200ms
slow-test: 1500ms bin=sample test=slow::fails
slow-test: 250ms bin=shard-1-of-2 test=other::case
slow-test: 201ms bin=sample test=boundary::over
"
    );

    let invalid = run_slow_tests(&log, "200ms");
    assert_eq!(invalid.status.code(), Some(2));
    let _ = fs::remove_dir_all(&dir);
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
            "unset SECONDS; readonly SECONDS=0; source \"$1\"; phase_log_prefix=coverage; PHASE_TIMING_LOG=\"$2\"; phase_begin provision; phase_end provision; phase_begin build-cli; phase_end build-cli; phase_report",
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
        "#!/usr/bin/env bash\nprintf '%s\\n' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\n",
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
        "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \"${1:-}\" == +* ]]; then shift; fi\ncmd=\"${1:-}\"; shift || true\ncase \"$cmd\" in\n  llvm-cov)\n    sub=\"${1:-}\"; shift || true\n    case \"$sub\" in\n      --version) exit 0 ;;\n      show-env) printf '%s\\n' true ;;\n      report)\n        out=\"\"\n        while [ $# -gt 0 ]; do\n          if [ \"$1\" = --output-path ]; then out=\"$2\"; shift 2; else shift; fi\n        done\n        mkdir -p \"$(dirname \"$out\")\"\n        printf '%s\\n' '{\"data\":[{\"totals\":{\"regions\":{\"covered\":1,\"count\":2},\"functions\":{\"covered\":1,\"count\":2},\"lines\":{\"covered\":1,\"count\":2},\"branches\":{\"covered\":1,\"count\":2}}}]}' >\"$out\"\n        ;;\n      *) echo \"unexpected llvm-cov $sub\" >&2; exit 2 ;;\n    esac\n    ;;\n  build)\n    printf '%s\\n' \"$*\" >>\"$STUB_BUILD_LOG\"\n    echo \"cargo-rustflags:${RUSTFLAGS-}\" >&2\n    echo \"cargo: visible stderr $*\" >&2\n    for need in \" -p krusty \" \" -p krusty-cli \" \" -p krusty-lsp \" \" --lib \" \" --bins \" \" --tests \"; do\n      case \" $* \" in\n        *\"$need\"*) ;;\n        *) echo \"unexpected build $*\" >&2; exit 2 ;;\n      esac\n    done\n    mkdir -p \"$CARGO_TARGET_DIR/coverage\"\n    printf '%s\\n' '#!/bin/sh' 'exit 0' >\"$CARGO_TARGET_DIR/coverage/krusty\"\n    printf '%s\\n' '#!/bin/sh' 'exit 0' >\"$CARGO_TARGET_DIR/coverage/krusty-lsp\"\n    chmod +x \"$CARGO_TARGET_DIR/coverage/krusty\" \"$CARGO_TARGET_DIR/coverage/krusty-lsp\"\n    if [[ \" $* \" == *\" --message-format=json \"* ]]; then\n      printf '%s\\n' \"{\\\"reason\\\":\\\"compiler-artifact\\\",\\\"package_id\\\":\\\"path+file:///workspace#krusty@0.0.1\\\",\\\"profile\\\":{\\\"test\\\":true},\\\"target\\\":{\\\"name\\\":\\\"e2e\\\",\\\"kind\\\":[\\\"test\\\"]},\\\"executable\\\":\\\"$E2E_BIN\\\"}\"\n      printf '%s\\n' \"{\\\"reason\\\":\\\"compiler-artifact\\\",\\\"package_id\\\":\\\"path+file:///workspace/crates/krusty-lsp#0.0.1\\\",\\\"profile\\\":{\\\"test\\\":true},\\\"target\\\":{\\\"name\\\":\\\"lsp-unit\\\",\\\"kind\\\":[\\\"lib\\\"]},\\\"executable\\\":\\\"$UNIT_BIN\\\"}\"\n    fi\n    ;;\n  test)\n    echo \"cargo: visible stderr $*\" >&2\n    if [[ \" $* \" == *\" --test e2e \"* ]]; then\n      printf '%s\\n' \"{\\\"profile\\\":{\\\"test\\\":true},\\\"executable\\\":\\\"$E2E_BIN\\\"}\"\n    else\n      printf '%s\\n' \"{\\\"profile\\\":{\\\"test\\\":true},\\\"executable\\\":\\\"$UNIT_BIN\\\"}\"\n    fi\n    ;;\n  *) echo \"unexpected cargo $cmd $*\" >&2; exit 2 ;;\nesac\n",
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
        "coverage must compile and discover its executables in one cargo invocation: {builds}"
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
            "build-compiler",
            "test-lsp-unit",
            "test-e2e",
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

    fs::write(
        &unit_bin,
        "#!/usr/bin/env bash\nprintf '%s\\n' 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\nexit 7\n",
    )
    .expect("write failing unit stub");
    let failed_unit = run_coverage("nproc");
    assert_eq!(failed_unit.status.code(), Some(1));
    let failed_unit_stderr =
        String::from_utf8(failed_unit.stderr).expect("coverage stderr is UTF-8");
    assert!(
        failed_unit_stderr.contains("coverage: lsp-unit exited with status 7"),
        "coverage hid unit failure: {failed_unit_stderr}"
    );

    fs::write(
        &unit_bin,
        "#!/usr/bin/env bash\nprintf '%s\\n' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\n",
    )
    .expect("restore unit stub");
    fs::write(
        &e2e_bin,
        "#!/usr/bin/env bash\nprintf '%s\\n' 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'\nexit 9\n",
    )
    .expect("write failing e2e stub");
    let failed_e2e = run_coverage("nproc");
    assert_eq!(failed_e2e.status.code(), Some(1));
    let failed_e2e_stderr = String::from_utf8(failed_e2e.stderr).expect("coverage stderr is UTF-8");
    assert!(
        failed_e2e_stderr.contains("coverage: e2e exited with status 9"),
        "coverage hid e2e failure: {failed_e2e_stderr}"
    );
    fs::remove_dir_all(temp).expect("remove coverage phase directory");
}

#[test]
fn coverage_profile_disables_intra_crate_thin_lto() {
    let manifest = include_str!("../Cargo.toml");
    let coverage = manifest
        .split_once("[profile.coverage]")
        .expect("coverage profile")
        .1;
    let body = coverage
        .split_once("\n[")
        .map(|(body, _)| body)
        .unwrap_or(coverage);
    assert!(
        body.lines().any(|line| line.trim() == "lto = \"off\""),
        "the coverage profile must set lto = \"off\" so Cargo passes -C lto=off and rustc skips intra-crate ThinLTO"
    );
}
