use super::common;

/// Every method's `MethodParameters` rows as `name/flags`, keyed by `name+descriptor`.
fn method_parameters(bytes: &[u8], file_name: &str) -> Vec<String> {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);

    let mut out = Vec::new();
    let mut member = String::new();
    let mut rows: Option<Vec<String>> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if line.starts_with("  ") && !line.starts_with("   ") && trimmed.contains('(') {
            if let Some(collected) = rows.take() {
                out.push(format!("{member} -> {}", collected.join(",")));
            }
            member = trimmed.to_string();
        }
        if trimmed == "MethodParameters:" {
            rows = Some(Vec::new());
            continue;
        }
        if let Some(collected) = rows.as_mut() {
            if trimmed.starts_with("Name") && trimmed.contains("Flags") {
                continue;
            }
            // A parameter row is `<name>` or `<name>  <flags>`; anything else ends the attribute.
            let mut fields = trimmed.split_whitespace();
            match (fields.next(), fields.next(), fields.next()) {
                (Some(name), flags, None)
                    if name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '$' || c == '_') =>
                {
                    collected.push(format!("{name}/{}", flags.unwrap_or("-")));
                }
                _ => {
                    out.push(format!("{member} -> {}", collected.join(",")));
                    rows = None;
                }
            }
        }
    }
    if let Some(collected) = rows {
        out.push(format!("{member} -> {}", collected.join(",")));
    }
    out.sort();
    out
}

fn kotlinc_classes(source_name: &str, source: &str) -> Option<std::path::PathBuf> {
    let root = common::scratch_dir().expect("reference scratch dir");
    let out = root.join("classes");
    std::fs::create_dir_all(&out).expect("reference output dir");
    let source_path = root.join(format!("{source_name}.kt"));
    std::fs::write(&source_path, source).expect("reference source");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        "-java-parameters".to_string(),
        source_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    Some(out)
}

const SOURCE: &str = "package demo\n\
    data class Acc(val redirectTo: String, val count: Int)\n\
    class Leaf {\n\
    \x20 suspend fun pull(id: String): String = id\n\
    }\n\
    class Svc(private val dep: String, private val leaf: Leaf) {\n\
    \x20 fun plain(one: String, two: Int): String = one + two\n\
    \x20 suspend fun waits(one: String): String {\n\
    \x20\x20 val first = leaf.pull(one)\n\
    \x20\x20 return first + dep\n\
    \x20 }\n\
    }\n\
    fun topLevel(a: String, b: Int): String = a + b\n";

/// `-java-parameters` makes kotlinc write a `MethodParameters` attribute naming each declared
/// parameter — including `$completion`, the continuation a `suspend fun` appends. krusty accepted the
/// flag and ignored it, so every class of a module built with it differed. A framework that reads
/// parameter names by reflection needs the attribute, and byte parity needs it exactly where kotlinc
/// puts it: after the annotation attributes, and never on a `$default` bridge or a synthetic marker
/// constructor.
#[test]
fn java_parameters_names_every_declared_parameter() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(reference_dir) = kotlinc_classes("JavaParameters", SOURCE) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classes = common::compile_in_process_files_java_parameters(
        &[("JavaParameters", SOURCE)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile with -java-parameters");

    let mut compared = 0;
    let mut rows = 0;
    for (name, ours) in &classes {
        if !name.ends_with("Kt") && name.contains("META-INF") {
            continue;
        }
        let class_file = reference_dir.join(format!("{name}.class"));
        if !class_file.exists() {
            continue;
        }
        let simple = name.rsplit('/').next().expect("simple name");
        let reference = std::fs::read(&class_file)
            .unwrap_or_else(|error| panic!("read kotlinc {name}: {error}"));
        let reference_rows = method_parameters(&reference, &format!("ref-{simple}.class"));
        rows += reference_rows.len();
        compared += 1;
        assert_eq!(
            method_parameters(ours, &format!("ours-{simple}.class")),
            reference_rows,
            "{name}: MethodParameters must match kotlinc"
        );
    }
    assert!(
        compared >= 3,
        "expected every fixture class to be compared, got {compared}"
    );
    assert!(
        rows >= 6,
        "fixture must exercise several parameter lists, got {rows}"
    );
    let _ = std::fs::remove_dir_all(reference_dir);
}
