//! Execute the pinned upstream generator declarations with their original model API defaults.
//! Only output locations and annotation construction are replaced; model selection is unchanged.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::conformance::TestTarget;

use super::{box_inventory, common};

const ENUM_SOURCE: &str = "compiler/test-infrastructure-utils.common/testFixtures/org/jetbrains/kotlin/test/TargetBackend.kt";
const MODEL_DSL: &str = "generators/test-generator/testFixtures/org/jetbrains/kotlin/generators/dsl/TestGenerationDSL.kt";
const GENERATOR_UTIL: &str = "generators/test-generator/testFixtures/org/jetbrains/kotlin/generators/util/TestGeneratorUtil.kt";
const CUSTOM_DATA: &str = "compiler/test-infrastructure-utils/testFixtures/org/jetbrains/kotlin/test/utils/CustomTestDataUtils.kt";
const SPEC_CONFIG: &str =
    "compiler/tests-spec/testFixtures/org/jetbrains/kotlin/spec/utils/GeneralConfiguration.kt";
const SPEC_HELPER: &str = "compiler/tests-spec/testFixtures/org/jetbrains/kotlin/spec/utils/tasks/GenerateSpecTestsUtils.kt";

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
    compile_and_run_with_options(root, source, None, &[])
}

fn compile_and_run_with_options(
    root: &Path,
    source: &str,
    java: Option<String>,
    options: &[&str],
) -> String {
    let work = Scratch::new();
    let enum_file = work.0.join("TargetBackend.kt");
    let probe_file = work.0.join("SelectionOracle.kt");
    std::fs::write(&enum_file, read(root, ENUM_SOURCE)).unwrap();
    std::fs::write(&probe_file, source).unwrap();
    let output = work.0.join("out");
    let stdlib = common::stdlib_jar();
    let mut arguments = vec![
        "-no-stdlib".into(),
        "-no-reflect".into(),
        "-classpath".into(),
        stdlib.to_string_lossy().into_owned(),
        "-d".into(),
        output.to_string_lossy().into_owned(),
        enum_file.to_string_lossy().into_owned(),
        probe_file.to_string_lossy().into_owned(),
    ];
    arguments.extend(options.iter().copied().map(str::to_owned));
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
        let actual_files: BTreeSet<_> = box_inventory::model_files(&corpus, target).unwrap();
        assert_eq!(
            actual_files, expected_files,
            "{name}: exact upstream model selection"
        );
    }
}

#[test]
fn native_configuration_matches_upstream_relevance_filters() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let root = box_inventory::checkout(&corpus).unwrap();
    let original = read(
        root,
        "native/native.tests/testFixtures/org/jetbrains/kotlin/konan/test/blackbox/support/group/ExtTestCaseGroupProvider.kt",
    );
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

#[test]
fn native_def_applicability_matches_the_pinned_runner() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let root = box_inventory::checkout(&corpus).unwrap();
    let original = read(
        root,
        "native/native.tests/testFixtures/org/jetbrains/kotlin/konan/test/blackbox/support/util/CInteropUtils.kt",
    );
    let function = &original[original
        .find("fun String.defFileContentsIsSupportedOn(")
        .unwrap()..];
    let cases = [
        "headers = stdlib.h\n",
        "language = Objective-C\nheaders = Test.h\n",
        "LANGUAGE=objective-c\n",
        "language = C\n",
        "---\nlanguage = Objective-C\n",
        "language = Objective-C\n---\n",
        "language = Objective-C = extra\n",
    ];
    let queries = [false, true]
        .into_iter()
        .flat_map(|apple| {
            cases.iter().map(move |source| {
                format!(
                    "{}.defFileContentsIsSupportedOn(KonanTarget(Family({apple}))).toString()",
                    kotlin_string(source)
                )
            })
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let kotlin = format!(
        r#"
class Family(val isAppleFamily: Boolean)
class KonanTarget(val family: Family)
{function}
fun box(): String = listOf({queries}).joinToString("\n")
"#
    );
    let result = compile_and_run(root, &kotlin);
    let actual = result.lines().collect::<Vec<_>>();
    let expected = [false, true]
        .into_iter()
        .flat_map(|apple| {
            cases.iter().map(move |source| {
                krusty::conformance::native_def_contents_supported(source, apple).to_string()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn native_target_conditions_match_the_pinned_evaluator() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let root = box_inventory::checkout(&corpus).unwrap();
    let original = read(
        root,
        "native/native.tests/testFixtures/org/jetbrains/kotlin/konan/test/blackbox/support/group/IgnoreDisableHelper.kt",
    );
    let properties = read(
        root,
        "native/native.tests/testFixtures/org/jetbrains/kotlin/konan/test/blackbox/support/ConfigurationProperties.kt",
    );
    let start = properties.find("enum class ClassLevelProperty(").unwrap();
    let end = start + properties[start..].find("\n    ;").unwrap();
    let property_enum = format!("{}\n}}", &properties[start..end]);
    let constants_start = original.find("private val TARGET_FAMILY").unwrap();
    let constants_end = original.find("internal val List<TargetBackend>").unwrap();
    let evaluator_start = original.find("internal fun Settings.evaluate(").unwrap();
    // The evaluator is the top-level function's text up to its closing brace at column 0; later
    // releases follow it with more declarations, earlier ones end the file there.
    let evaluator_end = evaluator_start + original[evaluator_start..].find("\n}\n").unwrap() + 3;
    let (arch, other, target, other_target) = match std::env::consts::ARCH {
        "aarch64" => ("ARM64", "X64", "linux_arm64", "linux_x64"),
        "x86_64" => ("X64", "ARM64", "linux_x64", "linux_arm64"),
        architecture => panic!("unsupported Native test architecture: {architecture}"),
    };
    let cases = vec![
        vec![String::new()],
        vec!["isAppleTarget=false".into()],
        vec!["isAppleTarget=true".into()],
        vec!["targetFamily=LINUX".into()],
        vec!["targetFamily=MINGW".into()],
        vec![format!("targetArchitecture={arch}")],
        vec![format!("targetArchitecture={other}")],
        vec![format!("target={target}")],
        vec![format!("target={other_target}")],
        vec!["targetSupportsCoreSymbolication=false".into()],
        vec!["targetSupportsCoreSymbolication=true".into()],
        vec!["targetFamily=LINUX && isAppleTarget=false".into()],
        vec!["targetFamily=LINUX && isAppleTarget=true".into()],
        vec!["isAppleTarget=true".into(), "targetFamily=LINUX".into()],
    ];
    let queries = cases
        .iter()
        .map(|values| {
            format!(
                "excluded(listOf({})).toString()",
                values
                    .iter()
                    .map(|value| kotlin_string(value))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let kotlin = include_str!("fixtures/box_selection/NativeDisableOracle.kt")
        .replace("@CLASS_LEVEL_PROPERTIES@", &property_enum)
        .replace("@ARCH@", arch)
        .replace("@TARGET@", target)
        .replace("@CONSTANTS@", &original[constants_start..constants_end])
        .replace("@EVALUATOR@", &original[evaluator_start..evaluator_end])
        .replace("@QUERIES@", &queries);
    // The pinned test infrastructure uses the bracket destructuring syntax ahead of its
    // default language version. Enable that syntax without rewriting the original evaluator.
    let result = compile_and_run_with_options(
        root,
        &kotlin,
        None,
        &["-XXLanguage:+NameBasedDestructuring"],
    );
    let results = result.lines().collect::<Vec<_>>();
    assert_eq!(results.len(), cases.len());
    for (values, excluded) in cases.iter().zip(results) {
        for name in ["DISABLE_NATIVE", "IGNORE_NATIVE"] {
            let source = values
                .iter()
                .map(|value| {
                    if value.is_empty() {
                        format!("// {name}\n")
                    } else {
                        format!("// {name}: {value}\n")
                    }
                })
                .collect::<String>();
            assert_eq!(
                krusty::conformance::native_box_configuration_applicable(&source),
                !excluded.parse::<bool>().unwrap(),
                "{source}"
            );
        }
    }
}
