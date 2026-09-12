use super::common;

/// The continuation's spill fields (`L$N` / `Z$N` / `I$N` …), in declaration order.
fn spill_fields(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let field = line.trim().strip_suffix(';')?;
            let (_, name) = field.rsplit_once(' ')?;
            (name.len() > 2 && name[1..].starts_with('$')).then(|| name.to_string())
        })
        .collect()
}

fn javap_class(bytes: &[u8], file_name: &str) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);
    text
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

/// A `when` that assigns its result temp in EVERY branch kills whatever the temp held on entry, so a
/// suspension inside a branch's value crosses nothing: kotlinc gives that temp no spill field. krusty
/// used to keep it live — any read after the `when` counted, even though every path to that read
/// overwrites the temp first — and spilled one `L$N` more than kotlinc on every such continuation.
#[test]
fn a_when_overwriting_its_result_temp_spills_no_field_for_it() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package demo\n\
        class Store {\n\
        \x20 suspend fun find(name: String): String? = name.takeIf { it.isNotEmpty() }\n\
        }\n\
        class Branches(private val store: Store) {\n\
        \x20 suspend fun pick(id: String, retry: Boolean): String {\n\
        \x20\x20 val fallback = \"a\"\n\
        \x20\x20 val found = store.find(id)\n\
        \x20\x20 return when {\n\
        \x20\x20\x20 found != null -> found\n\
        \x20\x20\x20 retry -> store.find(id) ?: fallback\n\
        \x20\x20\x20 else -> fallback\n\
        \x20\x20 }\n\
        \x20 }\n\
        }\n";
    let Some(reference) = kotlinc_class("WhenSpill", source, "demo/Branches$pick$1") else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classes = common::compile_in_process_files(
        &[("WhenSpill", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile when-branch continuation");
    let ours = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Branches$pick$1").then_some(bytes))
        .expect("pick continuation");

    let ours_text = javap_class(ours, "ours-Branches$pick$1.class");
    let reference_text = javap_class(&reference, "ref-Branches$pick$1.class");
    assert_eq!(
        spill_fields(&ours_text),
        spill_fields(&reference_text),
        "spill fields must match kotlinc\nkrusty:\n{ours_text}\nkotlinc:\n{reference_text}"
    );
}
