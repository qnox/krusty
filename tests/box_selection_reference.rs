//! Execute the pinned upstream generator declarations with their original model API defaults.
//! Only output locations and annotation construction are replaced; model selection is unchanged.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::conformance::TestTarget;

use super::{box_inventory, common};

const ENUM_SOURCE: &str =
    "compiler/test-infrastructure-utils.common/testFixtures/org/jetbrains/kotlin/test/TargetBackend.kt";
const MODEL_DSL: &str =
    "generators/test-generator/testFixtures/org/jetbrains/kotlin/generators/dsl/TestGenerationDSL.kt";
const GENERATOR_UTIL: &str =
    "generators/test-generator/testFixtures/org/jetbrains/kotlin/generators/util/TestGeneratorUtil.kt";
const CUSTOM_DATA: &str =
    "compiler/test-infrastructure-utils/testFixtures/org/jetbrains/kotlin/test/utils/CustomTestDataUtils.kt";
const SPEC_CONFIG: &str =
    "compiler/tests-spec/testFixtures/org/jetbrains/kotlin/spec/utils/GeneralConfiguration.kt";
const SPEC_HELPER: &str =
    "compiler/tests-spec/testFixtures/org/jetbrains/kotlin/spec/utils/tasks/GenerateSpecTestsUtils.kt";

fn read(root: &Path, relative: &str) -> String {
    std::fs::read_to_string(root.join(relative))
        .unwrap_or_else(|error| panic!("read pinned selector source {relative}: {error}"))
}

fn kotlin_string(value: &str) -> String {
    let mut result = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            '"' => result.push_str("\\\""),
            '$' => result.push_str("\\$"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            other => result.push(other),
        }
    }
    result.push('"');
    result
}

fn constants(source: &str) -> String {
    source
        .lines()
        .filter_map(|line| {
            line.find("const val ")
                .map(|start| line[start..].to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn type_arguments(source: &str, call: &str) -> BTreeSet<String> {
    let prefix = format!("{call}<");
    source
        .split(&prefix)
        .skip(1)
        .map(|tail| {
            let name = tail.split_once('>').expect("closed upstream test type").0;
            assert!(
                name.chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'),
                "unsupported upstream generator type {name}"
            );
            name.to_owned()
        })
        .collect()
}

fn main_function(source: &str) -> &str {
    let start = source
        .find("fun main(args: Array<String>) {")
        .expect("upstream generator has its original main entry");
    let tail = &source[start..];
    let end = tail
        .find("\n}")
        .expect("upstream generator main closes at top level")
        + 2;
    &tail[..end]
}

fn model_signature(source: &str) -> &str {
    let start = source
        .find("        fun model(")
        .expect("upstream model API");
    let tail = &source[start..];
    let end = tail
        .find("\n        ) {")
        .expect("upstream model API signature end")
        + "\n        )".len();
    tail[..end].trim()
}

fn helper_function(source: &str) -> &str {
    let start = source
        .find("fun detectDirsWithTestsMapFileOnly(")
        .expect("upstream spec helper");
    let tail = &source[start..];
    let end = tail.find("\n}").expect("upstream spec helper closes") + 2;
    &tail[..end]
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        Self(common::scratch_dir().expect("allocate selector oracle"))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_and_run(root: &Path, source: &str) -> String {
    compile_and_run_with_java(root, source, None)
}

fn compile_and_run_with_java(root: &Path, source: &str, java: Option<String>) -> String {
    let work = Scratch::new();
    let enum_file = work.0.join("TargetBackend.kt");
    let probe_file = work.0.join("SelectionOracle.kt");
    std::fs::write(&enum_file, read(root, ENUM_SOURCE)).unwrap();
    std::fs::write(&probe_file, source).unwrap();
    let output = work.0.join("out");
    let stdlib = common::stdlib_jar();
    let arguments = vec![
        "-no-stdlib".into(),
        "-no-reflect".into(),
        "-classpath".into(),
        stdlib.to_string_lossy().into_owned(),
        "-d".into(),
        output.to_string_lossy().into_owned(),
        enum_file.to_string_lossy().into_owned(),
        probe_file.to_string_lossy().into_owned(),
    ];
    let (status, diagnostics) = common::kotlinc_server::kotlinc_compile_unrecorded(&arguments)
        .expect("exact-version reference compiler");
    assert_eq!(status, 0, "selector oracle compilation:\n{diagnostics}");
    let mut classpath = vec![output.clone(), stdlib, common::jdk_modules()];
    if let Some(java) = java {
        let (classes, _) =
            common::javac_compile(&[("DirectiveOracle.java".into(), java)], &classpath[..2])
                .expect("compile original upstream directive methods");
        classpath.push(classes);
    }
    common::run_box(&[], "SelectionOracleKt", &classpath)
        .expect("run selector oracle on the shared JVM")
}

fn selector_output(root: &Path, target: &str, flag: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/box_selection.py");
    let output = Command::new("python3")
        .arg(script)
        .arg("--checkout")
        .arg(root)
        .args(["--target", target])
        .args((!flag.is_empty()).then_some(flag))
        .output()
        .expect("run generator extraction");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn decode_hex(value: &str) -> String {
    assert!(value.is_ascii() && value.len().is_multiple_of(2));
    let bytes: Vec<_> = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap()
}

fn oracle_source(root: &Path, target: &str, generator: &str) -> String {
    let original = read(root, generator);
    let main = main_function(&original);
    let classes = type_arguments(main, "testClass")
        .union(&type_arguments(main, "provider"))
        .map(|name| format!("class {name}"))
        .collect::<Vec<_>>()
        .join("\n");
    let spec = helper_function(&read(root, SPEC_HELPER)).to_owned();
    let spec = format!(
        "object SpecHelpers {{\n{spec}\n}}\n\
         fun detectDirsWithTestsMapFileOnly(name: String): List<String> = \
         SpecHelpers.detectDirsWithTestsMapFileOnly(name, checkoutRoot)"
    );
    include_str!("fixtures/box_selection/GeneratorOracle.kt")
        .replace("__CHECKOUT__", &kotlin_string(&root.to_string_lossy()))
        .replace("__TARGET__", &kotlin_string(target))
        .replace(
            "__MODEL_SIGNATURE__",
            model_signature(&read(root, MODEL_DSL)),
        )
        .replace(
            "__GENERATOR_CONSTANTS__",
            &constants(&read(root, GENERATOR_UTIL)),
        )
        .replace(
            "__OTHER_CONSTANTS__",
            &format!(
                "{}\n{}",
                constants(&read(root, CUSTOM_DATA)),
                constants(&read(root, SPEC_CONFIG))
            ),
        )
        .replace("__RUNNER_CLASSES__", &classes)
        .replace("__SPEC_HELPER__", &spec)
        .replace("__GENERATOR_MAIN__", main)
}

#[test]
fn every_box_root_and_selected_file_matches_executed_upstream_generators() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let root = box_inventory::checkout(&corpus).unwrap();
    for (target, name) in [
        (TestTarget::Jvm, "jvm"),
        (TestTarget::Native, "native"),
        (TestTarget::WasmJs, "wasm-js"),
        (TestTarget::WasmWasi, "wasm-wasi"),
    ] {
        let generators = selector_output(root, name, "--generators");
        let mut expected_roots = BTreeSet::new();
        let mut expected_files = BTreeSet::new();
        for generator in generators.lines().map(decode_hex) {
            let source = oracle_source(root, name, &generator);
            let result = compile_and_run(root, &source);
            for line in result.lines() {
                let (kind, encoded) = line
                    .split_once('\t')
                    .unwrap_or_else(|| panic!("upstream generator oracle failed: {line}"));
                let value = decode_hex(encoded);
                match kind {
                    "R" => {
                        expected_roots.insert(value);
                    }
                    "F" => {
                        expected_files.insert(root.join(value));
                    }
                    other => panic!("unknown generator oracle record {other}"),
                }
            }
        }
        let actual_roots: BTreeSet<_> = selector_output(root, name, "--required-roots")
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            actual_roots, expected_roots,
            "{name}: every upstream box directory"
        );
        let actual_files: BTreeSet<_> = box_inventory::generator_files(&corpus, target)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(
            actual_files, expected_files,
            "{name}: complete upstream case inventory"
        );
    }
}

fn java_method<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source.find(signature).expect("original directive method");
    let tail = &source[start..];
    let end = tail.find("\n    }").expect("directive method end") + "\n    }".len();
    &tail[..end]
}

#[test]
fn backend_directives_match_the_executed_upstream_matcher() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let root = box_inventory::checkout(&corpus).unwrap();
    let source = read(root, "compiler/test-infrastructure-utils/testFixtures/org/jetbrains/kotlin/test/InTextDirectivesUtils.java");
    let compatible = java_method(&source, "public static boolean isCompatibleTarget(\n");
    let recursive = java_method(
        &source,
        "private static boolean isCompatibleTargetExceptAny(",
    );
    let ignored = java_method(&source, "private static boolean isIgnoredTarget(@NotNull TargetBackend targetBackend, boolean includeAny, List<String> ignoredBackends)");
    let java = format!(
        "import java.util.*;\nimport java.util.stream.*;\nimport org.jetbrains.kotlin.test.TargetBackend;\n@interface NotNull {{}}\npublic class DirectiveOracle {{\n{compatible}\n{recursive}\n{ignored}\n\
         public static boolean check(String backend, String kind, String token) {{\n\
         TargetBackend target = TargetBackend.valueOf(backend);\n\
         List<TargetBackend> values = Collections.singletonList(TargetBackend.valueOf(token));\n\
         if (kind.equals(\"TARGET_BACKEND\")) return isCompatibleTarget(target, values, Collections.emptyList());\n\
         if (kind.equals(\"DONT_TARGET_EXACT_BACKEND\")) return isCompatibleTarget(target, Collections.emptyList(), values);\n\
         if (kind.equals(\"IGNORE_BACKEND_K1\") || kind.equals(\"IGNORE_BACKEND_K2_MULTI_MODULE\")) return true;\n\
         return !isIgnoredTarget(target, true, Collections.singletonList(token));\n}}\n}}"
    );
    let tokens = [
        "ANY",
        "JVM",
        "JVM_IR",
        "JS_IR",
        "WASM",
        "WASM_JS",
        "WASM_WASI",
        "NATIVE",
    ];
    let kinds = [
        "TARGET_BACKEND",
        "DONT_TARGET_EXACT_BACKEND",
        "IGNORE_BACKEND",
        "IGNORE_BACKEND_K2",
        "IGNORE_BACKEND_K1",
        "IGNORE_BACKEND_K2_MULTI_MODULE",
    ];
    let targets: &[(&str, &[&str])] = &[
        ("JVM_IR", &["JVM", "JVM_IR"]),
        ("NATIVE", &["NATIVE"]),
        ("WASM_JS", &["WASM", "WASM_JS"]),
        ("WASM_WASI", &["WASM", "WASM_WASI"]),
    ];
    let mut queries = Vec::new();
    let mut expected = Vec::new();
    for (backend, names) in targets {
        for kind in kinds {
            for token in tokens {
                queries.push(format!(
                    "method.invoke(null, {}, {}, {}).toString()",
                    kotlin_string(backend),
                    kotlin_string(kind),
                    kotlin_string(token)
                ));
                let input = format!("// {kind}: {token}");
                expected.push((
                    format!("{backend}: {input}"),
                    krusty::conformance::backend_applicable(&input, names),
                ));
            }
        }
    }
    let kotlin = format!("fun box(): String {{ val method = Class.forName(\"DirectiveOracle\").getMethod(\"check\", String::class.java, String::class.java, String::class.java); return listOf({}).joinToString(\"\\n\") }}", queries.join(",\n"));
    let result = compile_and_run_with_java(root, &kotlin, Some(java));
    let actual: Vec<_> = result.lines().collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "upstream directive oracle result: {result}"
    );
    let differences: Vec<_> = expected
        .iter()
        .zip(actual)
        .filter_map(|((label, ours), upstream)| {
            let upstream: bool = upstream.parse().expect("upstream boolean");
            (*ours != upstream).then(|| format!("{label}: harness={ours}, kotlinc={upstream}"))
        })
        .collect();
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn native_configuration_matches_upstream_relevance_filters() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let root = box_inventory::checkout(&corpus).unwrap();
    let original = read(root, "native/native.tests/testFixtures/org/jetbrains/kotlin/konan/test/blackbox/support/group/ExtTestCaseGroupProvider.kt");
    let constants = original
        .lines()
        .filter(|line| line.contains("private val INCOMPATIBLE_"))
        .collect::<Vec<_>>()
        .join("\n");
    let start = original.find("INCOMPATIBLE_DIRECTIVES.none").unwrap();
    let end = original[start..].find("&& !(FILECHECK_STAGE").unwrap() + start;
    let expression = original[start..end].trim();
    let cases = [
        ("FULL_JDK", ""),
        ("JVM_TARGET", "1.8"),
        ("DIAGNOSTICS", "-UNUSED_VARIABLE"),
        ("API_VERSION", "1.4"),
        ("API_VERSION", "1.5"),
        ("LANGUAGE_VERSION", "1.3"),
        ("LANGUAGE_VERSION", "1.4"),
        ("LANGUAGE_VERSION", "2.4"),
        ("WITH_STDLIB", ""),
        ("KJS_WITH_FULL_RUNTIME", ""),
    ];
    let queries = cases
        .iter()
        .map(|(name, value)| {
            format!(
                "relevant({}, {}).toString()",
                kotlin_string(name),
                kotlin_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let kotlin = format!(
        r#"
const val FULL_JDK = "FULL_JDK"
const val JVM_TARGET = "JVM_TARGET"
const val DIAGNOSTICS = "DIAGNOSTICS"
const val API_VERSION = "API_VERSION"
const val LANGUAGE_VERSION = "LANGUAGE_VERSION"
object ApiVersion {{ const val KOTLIN_1_4 = "1.4" }}
object LanguageVersion {{ const val KOTLIN_1_3 = "1.3"; const val KOTLIN_1_4 = "1.4" }}
{constants}
class Directives(val name: String, val value: String) {{
    operator fun contains(key: String) = key == name
    operator fun get(key: String): List<String> = if (key == name) listOf(value) else emptyList()
}}
class Structure(val directives: Directives)
fun relevant(name: String, value: String): Boolean {{
    val structure = Structure(Directives(name, value))
    return {expression}
}}
fun box(): String = listOf({queries}).joinToString("\n")
"#
    );
    let result = compile_and_run(root, &kotlin);
    let results: Vec<_> = result.lines().collect();
    assert_eq!(results.len(), cases.len());
    for ((name, value), actual) in cases.into_iter().zip(results) {
        let source = format!("// {name}: {value}");
        assert_eq!(
            krusty::conformance::native_box_configuration_applicable(&source),
            actual.parse::<bool>().unwrap(),
            "{source}"
        );
    }
}
