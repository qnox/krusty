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
    let classpath = vec![output.clone(), stdlib, common::jdk_modules()];
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
