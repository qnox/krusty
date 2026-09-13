use super::common;

fn spill_names(bytes: &[u8], file_name: &str) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);
    let row = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("n=["))
        .unwrap_or_else(|| panic!("no debug-metadata names in:\n{text}"));
    row.trim_start_matches("n=[")
        .trim_end_matches(']')
        .split(',')
        .map(|name| name.trim().trim_matches('"').to_string())
        .collect()
}

fn kotlinc_class(source_name: &str, source: &str, class_name: &str) -> Vec<u8> {
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
    let (code, stderr) = compiled.expect("reference kotlinc unavailable under the test harness");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_name}.class")))
        .unwrap_or_else(|error| panic!("read kotlinc class {class_name}: {error}"));
    let _ = std::fs::remove_dir_all(root);
    bytes
}

/// Every local an inline expansion materializes is in scope at a suspension inside the spliced body,
/// and kotlinc spills it under the callee's own source name with one `$iv` per expansion depth. krusty
/// dropped both the name and the scope entry: the callee's parameters became unnamed temps and the
/// cloned body declarations lost the names their originals carried, so the continuation's `n` array
/// held only the caller's own locals.
#[test]
fn a_spliced_inline_body_names_its_locals_with_iv() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let source = "package demo\n\
        class Builder {\n\
        \x20 var path: String = \"\"\n\
        }\n\
        class Client {\n\
        \x20 suspend fun send(spec: String): String = spec\n\
        }\n\
        inline fun buildSpec(urlString: String, block: Builder.() -> Unit): String {\n\
        \x20 val builder = Builder()\n\
        \x20 builder.block()\n\
        \x20 return urlString + builder.path\n\
        }\n\
        class Caller(private val client: Client) {\n\
        \x20 suspend fun nested(tag: String): String =\n\
        \x20\x20 client.send(buildSpec(\"root\") { path = client.send(tag) })\n\
        }\n";
    let reference = kotlinc_class("InlineSpliceNames", source, "demo/Caller$nested$1");
    let classes = common::compile_in_process_files(
        &[("InlineSpliceNames", source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile inline-splice continuation");
    let ours = classes
        .iter()
        .find_map(|(name, bytes)| (name == "demo/Caller$nested$1").then_some(bytes))
        .expect("nested continuation");

    let ours_names = spill_names(ours, "ours-Caller$nested$1.class");
    let reference_names = spill_names(&reference, "ref-Caller$nested$1.class");
    assert_eq!(
        ours_names, reference_names,
        "inline-expansion spill names and order must match kotlinc exactly"
    );
}
