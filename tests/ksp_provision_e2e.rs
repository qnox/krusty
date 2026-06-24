//! Real provisioning of the KSP toolchain: detect a host resolver and download a KSP artifact +
//! its transitive closure into a folder — the production path for acquiring the sidecar jars
//! (`docs/PLUGIN_API.md`, "Drop-in extension management").
//!
//! Network- and resolver-dependent, so it is opt-in: set `KRUSTY_PROVISION_E2E=1` to run it (the
//! routine gate stays fast). It also self-skips if no resolver is detected.

use krusty::plugins::deps;

#[test]
fn provisions_ksp_jars_via_detected_resolver() {
    if std::env::var("KRUSTY_PROVISION_E2E").is_err() {
        eprintln!("skipping: set KRUSTY_PROVISION_E2E=1 to run (network + resolver)");
        return;
    }
    let Some(resolver) = deps::detect() else {
        eprintln!("skipping: no gradle/mvn/cs resolver detected");
        return;
    };
    eprintln!("using resolver: {resolver:?}");

    let out = std::env::temp_dir().join(format!("krusty_ksp_libs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);

    // The KSP API jar (small) + its transitive closure proves the mechanism without pulling the full
    // ~60MB kotlin-compiler. A real run would also list symbol-processing-aa + kotlin-compiler.
    let coords = vec!["com.google.devtools.ksp:symbol-processing-api:2.0.21-1.0.28".to_string()];

    let jars = resolver
        .fetch(&coords, &out)
        .expect("resolver should download the KSP artifact closure");

    assert!(
        !jars.is_empty(),
        "expected jars materialized into {}",
        out.display()
    );
    assert!(
        jars.iter().any(|j| j
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains("symbol-processing-api"))),
        "the requested KSP api jar must be present; got {jars:?}"
    );
    eprintln!("provisioned {} jars into {}", jars.len(), out.display());
    let _ = std::fs::remove_dir_all(&out);
}
