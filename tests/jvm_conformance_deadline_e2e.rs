//! Cold required reference toolchains use the JVM matrix step's budget, not a global override.

#[test]
fn the_jvm_non_box_ci_step_owns_its_cold_toolchain_budget_and_preinstalled_jdk() {
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
    assert_eq!(
        environment.get("KSP_E2E_JDK").copied(),
        Some("${{ env.JAVA_HOME_21_X64 }}")
    );
    for required in [
        "KRUSTY_KSP_E2E",
        "KRUSTY_REQUIRE_KSP_E2E",
        "KRUSTY_REQUIRE_SERIALIZATION_CONFORMANCE",
    ] {
        assert_eq!(environment.get(required).copied(), Some("\"1\""));
    }
    assert_eq!(
        include_str!("../scripts/test-gate-defaults.sh")
            .lines()
            .find(|line| line.starts_with("export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS=")),
        Some("export KRUSTY_CONFORMANCE_TIMEOUT_SECONDS=\"${KRUSTY_CONFORMANCE_TIMEOUT_SECONDS:-120}\"")
    );
}
