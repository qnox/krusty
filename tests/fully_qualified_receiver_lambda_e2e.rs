use super::common;

/// A top-level function called by its FULLY-QUALIFIED name shapes its receiver lambda exactly as the
/// imported spelling does. krusty established the receiver only for the imported call, so every
/// member inside a fully-qualified builder block was "unresolved reference" — the shape
/// `kotlinx.serialization.json.Json { prettyPrint = true }` takes, which is why one corpus module
/// compiled to nothing.
#[test]
fn a_fully_qualified_call_shapes_its_receiver_lambda() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let library = "package pkg\n\
        class Cfg {\n\
        \x20 var pretty: Boolean = false\n\
        }\n\
        fun Build(action: Cfg.() -> Unit): Cfg {\n\
        \x20 val cfg = Cfg()\n\
        \x20 cfg.action()\n\
        \x20 return cfg\n\
        }\n";
    let user = "package demo\n\
        import pkg.Build\n\
        object Imported {\n\
        \x20 val c = Build { pretty = true }\n\
        }\n\
        object FullyQualified {\n\
        \x20 val c = pkg.Build { pretty = true }\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("Builder", library), ("User", user)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles a fully-qualified receiver-lambda call");
    for expected in ["demo/Imported", "demo/FullyQualified"] {
        assert!(
            classes.iter().any(|(name, _)| name == expected),
            "expected {expected} among {:?}",
            classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
        );
    }
}
