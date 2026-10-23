//! The complete Native corpus has a dedicated CI budget, not the ordinary test deadline.

#[test]
fn the_native_corpus_has_an_explicit_ci_deadline() {
    let workflow = include_str!("../.github/workflows/ci.yml");
    let step = workflow
        .split("      - name: run native box conformance ${{ matrix.version }}\n")
        .nth(1)
        .expect("the Native corpus step exists")
        .split("\n      - name:")
        .next()
        .unwrap();
    let environment = step.split("        env:\n").nth(1).map(|rest| {
        rest.split("        run: |\n")
            .next()
            .expect("the environment precedes the run command")
    });
    assert_eq!(
        environment,
        Some("          KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS: \"1800\"\n")
    );
}
