use super::common;
use std::collections::BTreeMap;

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
    out
}

fn parameters_by_method(rows: Vec<String>) -> BTreeMap<String, String> {
    let mut keyed = BTreeMap::new();
    for row in rows {
        let (method, parameters) = row
            .split_once(" -> ")
            .expect("MethodParameters row has a method key");
        assert!(
            keyed
                .insert(method.to_string(), parameters.to_string())
                .is_none(),
            "duplicate MethodParameters row for {method}"
        );
    }
    keyed
}

fn kotlinc_classes(source_name: &str, source: &str) -> std::path::PathBuf {
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
    ])
    .expect("reference kotlinc unavailable under the test harness");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    out
}

fn compile_java_parameters(
    source_name: &str,
    source: &str,
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Option<Vec<(String, Vec<u8>)>> {
    use krusty::diag::DiagSink;
    use krusty::source::SourceInput;

    let mut diagnostics = DiagSink::new();
    let stems = vec![source_name.to_string()];
    let inputs = vec![SourceInput::kotlin(source).with_file_stem(source_name)];
    let classpath = common::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
            .expect("JVM provider initialization"),
    );
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &krusty::features::LangFeatures::default(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );
    let backend = krusty::jvm::JvmBackend::new(classpath).with_java_parameters(true);
    let outputs =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diagnostics);
    (!diagnostics.has_errors()).then(|| {
        outputs
            .into_iter()
            .map(|(path, bytes)| {
                (
                    path.strip_suffix(".class").unwrap_or(&path).to_string(),
                    bytes,
                )
            })
            .collect()
    })
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

const GENERATED_SOURCE: &str = "package demo\n\
    enum class Entry(val code: Int) { ONE(1), TWO(2) }\n\
    data class GeneratedData(val text: String, val count: Int = 1)\n\
    class GeneratedOuter(val seed: Int) {\n\
    \x20 inner class Inner(val value: String)\n\
    \x20 constructor(seed: Int, suffix: String) : this(seed + suffix.length)\n\
    \x20 fun String.extension(step: Int = 1): String = this + step + seed\n\
    \x20 suspend fun suspended(input: String): String = input\n\
    }\n\
    @JvmInline value class Wrapped(val raw: String)\n\
    @JvmInline value class Rich(val raw: String) {\n\
    \x20 val size: Int get() = raw.length\n\
    \x20 fun append(suffix: String): String = raw + suffix\n\
    }\n\
    fun localFactory(seed: String): Any {\n\
    \x20 class Local(val count: Int) { fun text(): String = seed + count }\n\
    \x20 return Local(1)\n\
    }\n\
    fun anonymousFactory(seed: String): Any = object { fun text(): String = seed }\n\
    interface Face { fun declared(name: String): String = name }\n";

const ENUM_CONSTRUCTOR_SOURCE: &str = "package demo\n\
    enum class Shape(val label: String, val sides: Int) {\n\
    \x20 BOX(\"box\", 4),\n\
    \x20 BLANK,\n\
    \x20 NAMED(\"named\"),\n\
    \x20 SIDED(6);\n\
    \x20 constructor(label: String) : this(label, 1)\n\
    \x20 constructor(sides: Int = 0) : this(\"auto\", sides)\n\
    }\n";

fn assert_parameter_parity(source_name: &str, source: &str, expected_classes: &[&str]) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let reference_dir = kotlinc_classes(source_name, source);
    let classes = compile_java_parameters(
        source_name,
        source,
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile with -java-parameters");

    let mut rows = 0;
    let mut mismatches = Vec::new();
    for name in expected_classes {
        let reference = std::fs::read(reference_dir.join(format!("{name}.class")))
            .unwrap_or_else(|error| panic!("read kotlinc {name}: {error}"));
        let ours = classes
            .iter()
            .find_map(|(class, bytes)| (class == name).then_some(bytes))
            .unwrap_or_else(|| {
                panic!(
                    "krusty did not emit expected class {name}; emitted {:?}",
                    classes.iter().map(|(class, _)| class).collect::<Vec<_>>()
                )
            });
        let simple = name.rsplit('/').next().expect("simple name");
        let reference_rows = parameters_by_method(method_parameters(
            &reference,
            &format!("ref-{simple}.class"),
        ));
        rows += reference_rows.len();
        let ours_rows =
            parameters_by_method(method_parameters(ours, &format!("ours-{simple}.class")));
        if ours_rows != reference_rows {
            mismatches.push(format!(
                "{name}:\n  krusty:  {ours_rows:?}\n  kotlinc: {reference_rows:?}"
            ));
        }
    }
    assert!(
        rows > expected_classes.len(),
        "fixture must cover parameterized methods"
    );
    assert!(
        mismatches.is_empty(),
        "ordered MethodParameters attributes must match kotlinc exactly:\n{}",
        mismatches.join("\n")
    );
    let root = reference_dir
        .parent()
        .expect("reference output has a scratch root");
    let _ = std::fs::remove_dir_all(root);
}

/// `-java-parameters` makes kotlinc write a `MethodParameters` attribute naming each declared
/// parameter — including `$completion`, the continuation a `suspend fun` appends. krusty accepted the
/// flag and ignored it, so every class of a module built with it differed. A framework that reads
/// parameter names by reflection needs the attribute, and byte parity needs it exactly where kotlinc
/// puts it: after the annotation attributes, and never on a `$default` bridge or a synthetic marker
/// constructor.
#[test]
fn java_parameters_names_every_declared_parameter() {
    assert_parameter_parity(
        "JavaParameters",
        SOURCE,
        &["demo/Acc", "demo/Leaf", "demo/Svc", "demo/JavaParametersKt"],
    );
}

/// Compiler-generated methods and parameters have declaration-specific names and flags. Missing
/// the attribute is not an acceptable fallback: all supported generated shapes match kotlinc.
#[test]
fn java_parameters_names_generated_and_prefixed_parameters() {
    assert_parameter_parity(
        "JavaParametersGenerated",
        GENERATED_SOURCE,
        &[
            "demo/Entry",
            "demo/GeneratedData",
            "demo/GeneratedOuter",
            "demo/GeneratedOuter$Inner",
            "demo/Wrapped",
            "demo/Rich",
            "demo/JavaParametersGeneratedKt$anonymousFactory$1",
            "demo/JavaParametersGeneratedKt",
            "demo/Face",
            "demo/Face$DefaultImpls",
        ],
    );
}

/// An enum's SECONDARY constructors carry the same synthetic `(String, int)` prefix its primary
/// does, and kotlinc describes it: `$enum$name`, `$enum$ordinal`, then the source parameters, the
/// two synthetics flagged. krusty threaded the prefix into the DESCRIPTOR only, so the identity
/// description still counted the declared parameters alone and this path asserted out —
/// "secondary constructor identities must match its physical JVM parameters" — the moment
/// `-java-parameters` was on. Both an argument-less and a parameterized secondary are covered,
/// because only the second distinguishes "the prefix is described" from "the table is empty" — and
/// a DEFAULTED one, whose synthetic overload carries the prefix in its descriptor while kotlinc
/// gives it no `MethodParameters` at all.
#[test]
fn java_parameters_names_an_enum_secondary_constructors_prefix() {
    assert_parameter_parity(
        "JavaParametersEnumSecondary",
        ENUM_CONSTRUCTOR_SOURCE,
        &["demo/Shape"],
    );
}

#[test]
fn method_parameters_remain_opt_in() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classes = common::compile_in_process_files(
        &[(
            "JavaParametersDisabled",
            "package demo\nfun plain(value: String) = value\n",
        )],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile without -java-parameters");
    let (_, facade) = classes
        .iter()
        .find(|(name, _)| name == "demo/JavaParametersDisabledKt")
        .expect("expected facade");
    assert_eq!(
        parameters_by_method(method_parameters(facade, "java-parameters-disabled.class")),
        BTreeMap::new()
    );
}
