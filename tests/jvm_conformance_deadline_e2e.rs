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
fn the_plugin_host_lint_step_owns_the_required_ksp_integrations() {
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
        .expect("the plugin host integration step exists")
        .split("\n      - ")
        .next()
        .unwrap();
    assert_eq!(
        step.trim(),
        "run: cargo test -p krusty-plugin-host -- --include-ignored",
        "real plugin host integrations must remain unconditional and include ignored tests"
    );
}
