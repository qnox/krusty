//! Cold required reference toolchains use the JVM matrix step's budget, not a global override.

#[test]
fn the_jvm_non_box_ci_step_owns_its_cold_reference_toolchain_budget() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    let step = workflow
        .split("      - name: run non-box conformance ${{ matrix.version }}\n")
        .nth(1)
        .expect("the JVM non-box conformance step exists")
        .split("\n      - name:")
        .next()
        .unwrap();
    let environment = step
        .split("        env:\n")
        .nth(1)
        .expect("the step declares its environment")
        .split("        run:")
        .next()
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| line.trim().split_once(": ").expect("an environment entry"))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        environment
            .get("KRUSTY_CONFORMANCE_TIMEOUT_SECONDS")
            .copied(),
        Some("\"600\"")
    );
    for retired in ["KRUSTY_KSP_E2E", "KRUSTY_REQUIRE_KSP_E2E", "KSP_E2E_JDK"] {
        assert!(
            !environment.contains_key(retired),
            "retired matrix flag {retired}"
        );
    }
    assert_eq!(
        environment
            .get("KRUSTY_REQUIRE_SERIALIZATION_CONFORMANCE")
            .copied(),
        Some("\"1\"")
    );
    assert_eq!(
        include_str!("../scripts/test-gate-defaults.sh")
            .lines()
            .find(|line| line.starts_with("export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS=")),
        Some("export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS=\"${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-120}\"")
    );
}

#[test]
fn plugin_host_tests_run_under_their_compiler_owner() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    let lint = workflow
        .split("\n  lint:\n")
        .nth(1)
        .expect("the lint job exists")
        .split("\n  ci:\n")
        .next()
        .unwrap();
    let step = lint
        .split("      - name: plugin host tests\n")
        .nth(1)
        .expect("the standalone host test step exists")
        .split("\n      - ")
        .next()
        .unwrap();
    assert_eq!(
        step.trim(),
        "run: cargo test --profile gate -p krusty-plugin-host -- --include-ignored",
        "standalone host tests remain unconditional and include ignored tests"
    );

    let ci = workflow
        .split("\n  ci:\n")
        .nth(1)
        .expect("the compiler CI job exists")
        .split("\n  # Each artifact family")
        .next()
        .unwrap();
    let coverage = ci
        .split("      - name: coverage gate\n")
        .nth(1)
        .expect("the existing coverage step exists");
    assert_eq!(coverage.lines().next(), Some("        id: coverage"));
    assert_eq!(
        include_str!("../scripts/coverage.sh")
            .lines()
            .find(|line| line
                .trim_start()
                .starts_with("printf 'compiler_e2e_binary=")),
        Some("  printf 'compiler_e2e_binary=%s\\n' \"$e2e_bin\" >> \"$coverage_github_output\"")
    );
    let step = ci
        .split("      - name: compiler plugin integration tests\n")
        .nth(1)
        .expect("the compiler-owned integration step exists")
        .split("\n      - ")
        .next()
        .unwrap();
    assert_eq!(
        step.trim(),
        r#"env:
          E2E_BINARY: ${{ steps.coverage.outputs.compiler_e2e_binary }}
          RUST_MIN_STACK: "134217728"
          LLVM_PROFILE_FILE: ${{ runner.temp }}/plugin-host-compiler-%p-%m.profraw
        run: |
          set -euo pipefail
          source scripts/test-gate-defaults.sh
          source scripts/test-deadline.sh
          test -n "$E2E_BINARY" && test -x "$E2E_BINARY"
          ignored_tests="$("$E2E_BINARY" plugin_host_compiler_e2e:: --ignored --list --format=terse)"
          printf '%s\n' "$ignored_tests"
          grep -q ': test$' <<< "$ignored_tests"
          echo "plugin host compiler tests: $E2E_BINARY plugin_host_compiler_e2e:: --include-ignored --test-threads=1 (timeout=${KRUSTY_E2E_TIMEOUT_SECONDS}s)"
          run_with_deadline "$KRUSTY_E2E_TIMEOUT_SECONDS" "$E2E_BINARY" plugin_host_compiler_e2e:: --include-ignored --test-threads=1"#
    );
}
