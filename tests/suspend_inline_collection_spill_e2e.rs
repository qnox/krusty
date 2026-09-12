use super::common;

fn debug_metadata(bytes: &[u8], file_name: &str) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);
    let rows: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("s=[") || line.starts_with("n=["))
        .map(str::to_string)
        .collect();
    assert!(!rows.is_empty(), "no debug-metadata spill rows in:\n{text}");
    rows
}

fn kotlinc_class(source_name: &str, source: &str, class_name: &str) -> Option<Vec<u8>> {
    let root = common::scratch_dir().expect("reference scratch dir");
    let out = root.join("classes");
    std::fs::create_dir_all(&out).expect("reference output dir");
    let source_path = root.join(format!("{source_name}.kt"));
    std::fs::write(&source_path, source).expect("reference source");
    let compiled = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ]);
    let Some((code, stderr)) = compiled else {
        let _ = std::fs::remove_dir_all(root);
        return None;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_name}.class")))
        .unwrap_or_else(|error| panic!("read kotlinc class {class_name}: {error}"));
    let _ = std::fs::remove_dir_all(root);
    Some(bytes)
}

fn assert_spill_names(name: &str, source: &str, class_name: &str) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(reference) = kotlinc_class(name, source, class_name) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classes = common::compile_in_process_files(
        &[(name, source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile inline-collection continuation");
    let simple = class_name.rsplit('/').next().expect("simple class name");
    let ours = classes
        .iter()
        .find_map(|(cls, bytes)| (cls == class_name).then_some(bytes))
        .unwrap_or_else(|| panic!("{class_name} not emitted"));
    assert_eq!(
        debug_metadata(ours, &format!("ours-{simple}.class")),
        debug_metadata(&reference, &format!("ref-{simple}.class")),
        "{name}: continuation spill positions and names must match kotlinc"
    );
}

/// A suspension inside a spliced stdlib `map`/`flatMap` runs with the inline expansion's own locals
/// live, and kotlinc spills every one of them under its inline name: the receiver at each of the two
/// frames (`$this$map$iv`, `$this$mapTo$iv$iv`), the destination, the loop element, and the lambda's
/// own parameter. The iterator keeps a position without a name. krusty spilled only the enclosing
/// function's locals, so both the field count and the `s`/`n` arrays came out short.
#[test]
fn a_spliced_map_spills_its_inline_locals() {
    assert_spill_names(
        "MapSpills",
        "package demo\n\
        class Source {\n\
        \x20 suspend fun load(id: String): String = id\n\
        }\n\
        class Fan(private val source: Source) {\n\
        \x20 suspend fun spread(ids: List<String>, tag: String): List<String> {\n\
        \x20\x20 val session = source.load(tag)\n\
        \x20\x20 return ids.map { one -> source.load(one + session) }\n\
        \x20 }\n\
        }\n",
        "demo/Fan$spread$1",
    );
}

/// `flatMap` names its element `element$iv$iv` where `map` names it `item$iv$iv`.
#[test]
fn a_spliced_flat_map_spills_its_inline_locals() {
    assert_spill_names(
        "FlatMapSpills",
        "package demo\n\
        class Source {\n\
        \x20 suspend fun load(id: String): List<String> = listOf(id)\n\
        }\n\
        class Fan(private val source: Source) {\n\
        \x20 suspend fun spread(ids: List<String>, tag: String): List<String> {\n\
        \x20\x20 val session = source.load(tag)\n\
        \x20\x20 return session.flatMap { one -> source.load(one) }\n\
        \x20 }\n\
        }\n",
        "demo/Fan$spread$1",
    );
}
