use super::common;
use std::path::PathBuf;

/// The newest `<group>/<artifact>` jar in the local Gradle cache, or `None` when it isn't there.
fn gradle_module_jar(group: &str, artifact: &str) -> Option<PathBuf> {
    let artifact_dir = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".gradle/caches/modules-2/files-2.1")
        .join(group)
        .join(artifact);
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for version in std::fs::read_dir(&artifact_dir).ok()?.flatten() {
        let name = version.file_name();
        let Some(name) = name.to_str() else { continue };
        let key = name
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>();
        let jar_name = format!("{artifact}-{name}.jar");
        for hash in std::fs::read_dir(version.path()).ok()?.flatten() {
            let jar = hash.path().join(&jar_name);
            if jar.is_file() && best.as_ref().is_none_or(|(current, _)| key > *current) {
                best = Some((key.clone(), jar));
            }
        }
    }
    best.map(|(_, jar)| jar)
}

/// `Row.serializer()` where `Row` is `@Serializable` in ANOTHER FILE of the same module. The
/// placeholder the plugin leaves for that accessor was only ever realized by searching the CURRENT
/// file's classes, so a sibling's accessor was never found: the placeholder survived to emission and
/// the backend declined the whole file — its module then emitted nothing at all.
#[test]
fn a_sibling_files_generated_serializer_is_reachable() {
    let Some(core) = gradle_module_jar("org.jetbrains.kotlinx", "kotlinx-serialization-core-jvm")
    else {
        eprintln!("skipping: kotlinx-serialization-core not in the Gradle cache");
        return;
    };
    let row = "package demo\n\
        import kotlinx.serialization.Serializable\n\
        @Serializable\n\
        data class Row(val id: String, val count: Int)\n";
    let writer = "package demo\n\
        import kotlinx.serialization.KSerializer\n\
        import kotlinx.serialization.builtins.ListSerializer\n\
        class Writer {\n\
        \x20 fun rows(): KSerializer<List<Row>> = ListSerializer(Row.serializer())\n\
        }\n";
    let classes = common::compile_in_process_files(
        &[("Row", row), ("Writer", writer)],
        &[core, common::stdlib_jar(), common::jdk_modules()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles a call to a sibling file's generated serializer");
    assert!(
        classes.iter().any(|(name, _)| name == "demo/Writer"),
        "expected demo/Writer among {:?}",
        classes.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
}
