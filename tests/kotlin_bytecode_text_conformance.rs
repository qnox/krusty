//! Kotlin's bytecode-text corpus, `compiler/testData/codegen/bytecodeText`, compiled by krusty.
//!
//! Each case is a box-style source whose comment lines `// <N> <pattern>` say how many times the
//! Java regular expression `(<pattern>)` occurs in the text ASM's `Textifier` prints for the
//! classes the case compiles to. That is JetBrains' `AbstractFirLightTreeBytecodeTextTest`; this
//! runner mirrors its `BytecodeTextHandler` and `BytecodeTextUtils`:
//!
//! - The suite's default directives are `WITH_STDLIB`, `WITH_REFLECT` and `ALLOW_KOTLIN_PACKAGE`.
//!   The stdlib is on every box compile's classpath already and the Kotlin-package permission
//!   follows the source, so only kotlin-reflect is added here.
//! - A module with one Kotlin file counts its expectations over every class it emitted except the
//!   coroutine `helpers/` ones, each preceded by an `@<path>.class` header line.
//! - A module with several Kotlin files (the coroutine helpers count as one) and no
//!   `TREAT_AS_ONE_FILE` counts each file's expectations over every class, except those after a
//!   `// @<path>.class:` line, which count over that one class alone.
//! - Expectations after `// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES` hold only when inline scope numbers
//!   are on, and that is a separate JetBrains configuration of the corpus; this lane runs the
//!   default one and skips them, exactly as the reference test does.
//!
//! Only Kotlin's classes are textified: a mixed case's javac output is not in the reference
//! compiler's class factory. Applicability and the compilation topology are the JVM box lane's.
//!
//! Outcomes ratchet like the CLI corpus. `tests/bytecode_text_expected_failures/jvm/<version>.txt`
//! lists the cases krusty does not reproduce yet; a case leaving or joining it fails until the list
//! is updated (`KRUSTY_BLESS_BYTECODE_TEXT_EXPECTATIONS=1` rewrites it from a full local run).
//! `tests/bytecode_text_expected_not_applicable/jvm/<version>.txt` lists the cases the reference
//! compiler does not reproduce through this runner, each under a comment saying why;
//! `the_reference_compiler_reproduces_every_applicable_bytecode_text_case` holds it honest.
//! `KRUSTY_BYTECODE_TEXT_ONLY=<substring>` runs a subset and reports each mismatch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use krusty::conformance::{BoxJdk, BoxJdkRoots};
use krusty::kotlin_version::KotlinVersion;

use super::asm_text;
use super::box_ratchet;
use super::box_reference_classes;
use super::common;
use super::kotlin_box_ir_jvm_conformance::{compile_box_case, CompiledClasses};

const CORPUS: &str = "compiler/testData/codegen/bytecodeText";
const FAILURES: &str = "bytecode_text_expected_failures";
const NOT_APPLICABLE: &str = "bytecode_text_expected_not_applicable";
const BLESS: &str = "KRUSTY_BLESS_BYTECODE_TEXT_EXPECTATIONS";

/// Classes under this prefix are the generated coroutine helpers; a single-file module's
/// expectations never count them.
const HELPERS_PREFIX: &str = "helpers/";
const JVM_IR_TEMPLATES: &str = "// JVM_IR_TEMPLATES";
const JVM_IR_TEMPLATES_WITH_INLINE_SCOPES: &str = "// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES";

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail(String),
    /// Outside the JVM backend: a target or mute directive excludes it.
    Excluded(&'static str),
    /// The runner itself could not evaluate the case.
    Unavailable(String),
}

/// One `// <N> <pattern>` line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    count: usize,
    needle: String,
    /// Declared under `// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES`.
    inline_scopes: bool,
}

impl Occurrence {
    fn render(&self) -> String {
        format!("{} {}", self.count, self.needle)
    }
}

/// Strip a leading `\s*` as `java.util.regex` defines it.
fn trim_java_space(text: &str) -> &str {
    text.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r'])
}

/// The text `^...(.*)$` captures at the end of a line: `.` stops at a line terminator, and `$`
/// may match only before a final one.
fn rest_of_line(text: &str) -> Option<&str> {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\r', '\u{85}', '\u{2028}', '\u{2029}']))
        .unwrap_or(text);
    (!body.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])).then_some(body)
}

/// `^\s*//\s*(\d+)\s*(.*)$`.
fn expected_occurrence(line: &str, inline_scopes: bool) -> Option<Occurrence> {
    let rest = trim_java_space(trim_java_space(line).strip_prefix("//")?);
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let count = rest[..digits].parse().ok()?;
    let needle = rest_of_line(trim_java_space(&rest[digits..]))?;
    Some(Occurrence {
        count,
        needle: needle.to_string(),
        inline_scopes,
    })
}

/// `^\s*//\s*@(.*):$`.
fn output_file(line: &str) -> Option<&str> {
    let rest = trim_java_space(trim_java_space(line).strip_prefix("//")?).strip_prefix('@')?;
    rest_of_line(rest)?.strip_suffix(':')
}

fn inline_scopes_after(line: &str, current: bool) -> bool {
    if line.contains(JVM_IR_TEMPLATES_WITH_INLINE_SCOPES) {
        true
    } else if line.contains(JVM_IR_TEMPLATES) {
        false
    } else {
        current
    }
}

fn expected_occurrences(content: &str) -> Vec<Occurrence> {
    let mut inline_scopes = false;
    let mut occurrences = Vec::new();
    for line in content.split('\n') {
        inline_scopes = inline_scopes_after(line, inline_scopes);
        occurrences.extend(expected_occurrence(line, inline_scopes));
    }
    occurrences
}

/// A multi-file module's expectations: those counted over every class, and those counted over
/// one output file each.
#[derive(Debug, Default, PartialEq, Eq)]
struct MultiFileOccurrences {
    global: Vec<Occurrence>,
    by_output: BTreeMap<String, Vec<Occurrence>>,
}

impl MultiFileOccurrences {
    fn read(&mut self, file: &str, content: &str) -> Result<(), String> {
        let mut inline_scopes = false;
        let mut output: Option<String> = None;
        for line in content.split('\n') {
            inline_scopes = inline_scopes_after(line, inline_scopes);
            if let Some(name) = output_file(line) {
                if self.by_output.contains_key(name) {
                    return Err(format!(
                        "{file}: expected occurrences for output file {name} were already provided"
                    ));
                }
                self.by_output.insert(name.to_string(), Vec::new());
                output = Some(name.to_string());
            }
            if let Some(occurrence) = expected_occurrence(line, inline_scopes) {
                match &output {
                    Some(name) => self.by_output.get_mut(name).unwrap().push(occurrence),
                    None => self.global.push(occurrence),
                }
            }
        }
        Ok(())
    }
}

/// A Kotlin source file of a test module.
struct TestFile {
    name: String,
    content: String,
    /// Supplied by the test infrastructure (the coroutine helpers), not by the case.
    additional: bool,
}

/// A module of a case, named as the compilation names its classes' module.
struct TestModule {
    name: String,
    files: Vec<TestFile>,
}

/// The modules and Kotlin files of `src`, in the topology the JVM box lane compiles it under.
fn test_modules(src: &str, stem: &str) -> Result<Vec<TestModule>, String> {
    let kotlin = |files: Vec<(String, String)>| -> Vec<TestFile> {
        files
            .into_iter()
            .map(|(name, content)| TestFile {
                name: format!("{name}.kt"),
                content,
                additional: false,
            })
            .collect()
    };
    let helpers = || TestFile {
        name: "CoroutineUtil.kt".to_string(),
        content: krusty::conformance::COROUTINE_HELPERS.to_string(),
        additional: true,
    };
    if src.contains("// MODULE:") {
        let mut modules = krusty::conformance::split_modules(src)
            .ok_or("the case's modules are outside the box compilation model")?;
        if krusty::conformance::directive(src, "WITH_COROUTINES") {
            krusty::conformance::inject_support_module(
                &mut modules,
                krusty::conformance::COROUTINE_HELPERS,
            );
        }
        return Ok(krusty::conformance::module_units(&modules)
            .into_iter()
            .map(|unit| {
                let support = unit.name == "support";
                let mut files = kotlin(unit.files);
                for file in &mut files {
                    file.additional = support;
                }
                TestModule {
                    name: unit.name,
                    files,
                }
            })
            .collect());
    }
    let mut files = if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
        let (blocks, java) = krusty::conformance::split_files(src);
        if blocks.is_empty() && java.is_empty() {
            kotlin(vec![(stem.to_string(), src.to_string())])
        } else {
            kotlin(blocks)
        }
    } else {
        kotlin(vec![(stem.to_string(), src.to_string())])
    };
    if src.contains("// WITH_COROUTINES") {
        files.push(helpers());
    }
    Ok(vec![TestModule {
        name: box_reference_classes::MAIN_MODULE.to_string(),
        files,
    }])
}

/// A Kotlin-compiled class of a module: its output path and its text.
struct OutputClass {
    path: String,
    text: String,
}

/// The module's Kotlin classes, in emission order; javac's output in a mixed case is dropped.
fn output_classes(classes: &[(&str, &[u8])]) -> Result<Vec<OutputClass>, CheckError> {
    let bytes: Vec<&[u8]> = classes.iter().map(|(_, bytes)| *bytes).collect();
    let texts = asm_text::textify(&bytes).map_err(|error| match error {
        asm_text::TextifyError::Unreadable(reason) => {
            CheckError::Failed(format!("ASM cannot read the emitted classes: {reason}"))
        }
        asm_text::TextifyError::Server(reason) => CheckError::Unavailable(reason),
    })?;
    Ok(classes
        .iter()
        .zip(texts)
        .filter(|(_, text)| {
            !text
                .source_file
                .as_deref()
                .is_some_and(|file| file.ends_with(".java"))
        })
        .map(|((name, _), text)| OutputClass {
            path: format!("{name}.class"),
            text: text.text,
        })
        .collect())
}

/// `ClassFileFactory.createText`: each class's text after an `@<path>` line.
fn create_text(classes: &[OutputClass], ignored_prefix: Option<&str>) -> String {
    let mut text = String::new();
    for class in classes {
        if ignored_prefix.is_some_and(|prefix| class.path.starts_with(prefix)) {
            continue;
        }
        text.push('@');
        text.push_str(&class.path);
        text.push('\n');
        text.push_str(&class.text);
    }
    text
}

/// `checkGeneratedTextAgainstExpectedOccurrences` under the default configuration (inline scope
/// numbers off). `None` when every counted expectation holds.
fn check_occurrences(text: &str, expected: &[Occurrence]) -> Result<Option<String>, CheckError> {
    let counted: Vec<&Occurrence> = expected
        .iter()
        .filter(|occurrence| !occurrence.inline_scopes)
        .collect();
    let needles: Vec<&str> = counted.iter().map(|o| o.needle.as_str()).collect();
    let counts = asm_text::count_matches(text, &needles).map_err(CheckError::Unavailable)?;
    let mismatches: Vec<String> = counted
        .iter()
        .zip(counts)
        .filter_map(|(occurrence, count)| match count {
            Some(count) if count == occurrence.count => None,
            Some(count) => Some(format!("expected {}, found {count}", occurrence.render())),
            None => Some(format!(
                "`{}` is not a valid Java pattern",
                occurrence.needle
            )),
        })
        .collect();
    Ok((!mismatches.is_empty()).then(|| mismatches.join("; ")))
}

/// Why a module's expectations could not be counted.
enum CheckError {
    /// The compiled classes themselves are at fault.
    Failed(String),
    /// The runner could not count them.
    Unavailable(String),
}

/// Every expectation of `module` against its `classes`. `Ok(None)` when all hold.
fn check_module(
    case: &str,
    module: &TestModule,
    classes: &[(&str, &[u8])],
) -> Result<Option<String>, CheckError> {
    let outputs = output_classes(classes)?;
    let mut problems = Vec::new();
    if module.files.len() > 1 && !krusty::conformance::directive(case, "TREAT_AS_ONE_FILE") {
        let mut expected = MultiFileOccurrences::default();
        for file in &module.files {
            if let Err(error) = expected.read(&file.name, &file.content) {
                problems.push(error);
            }
        }
        if !expected.global.is_empty() {
            problems.extend(check_occurrences(
                &create_text(&outputs, None),
                &expected.global,
            )?);
        }
        for (path, occurrences) in &expected.by_output {
            match outputs.iter().find(|class| &class.path == path) {
                Some(class) => problems.extend(
                    check_occurrences(&class.text, occurrences)?.map(|m| format!("{path}: {m}")),
                ),
                None => problems.push(format!("missing output file {path}, got {}", outputs.len())),
            }
        }
    } else if let Some(file) = module.files.iter().find(|file| !file.additional) {
        problems.extend(check_occurrences(
            &create_text(&outputs, Some(HELPERS_PREFIX)),
            &expected_occurrences(&file.content),
        )?);
    }
    Ok((!problems.is_empty()).then(|| {
        let mut message = problems.join("; ");
        if module.name != box_reference_classes::MAIN_MODULE {
            message = format!("module {}: {message}", module.name);
        }
        message
    }))
}

/// Check every module of a case against the classes a compiler emitted for it, looked up by
/// module name.
fn check_case<'a>(
    case: &str,
    modules: &[TestModule],
    classes_of: impl Fn(&str) -> Option<Vec<(&'a str, &'a [u8])>>,
) -> Outcome {
    let mut problems = Vec::new();
    for module in modules {
        let Some(classes) = classes_of(&module.name) else {
            problems.push(format!("module {} emitted no classes", module.name));
            continue;
        };
        match check_module(case, module, &classes) {
            Ok(None) => {}
            Ok(Some(problem)) | Err(CheckError::Failed(problem)) => problems.push(problem),
            Err(CheckError::Unavailable(error)) => return Outcome::Unavailable(error),
        }
    }
    if problems.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(problems.join("\n"))
    }
}

/// The corpus and the inputs every case compiles against.
struct Corpus {
    root: PathBuf,
    jdk_roots: BoxJdkRoots,
    reflect: PathBuf,
}

impl Corpus {
    fn new() -> Self {
        let box_dir = krusty::toolchain::box_corpus_dir()
            .expect("the Kotlin box corpus is provisioned (`just box-corpus <version>`)");
        // `just box-corpus` checks the bytecode corpora out beside codegen/box.
        let root = box_dir
            .parent()
            .expect("codegen/box has a parent")
            .join("bytecodeText");
        assert!(
            root.is_dir(),
            "{} is missing; re-provision the checkout with `just box-corpus <version>`",
            root.display()
        );
        let java_home = common::java_home();
        let jdk_roots = BoxJdkRoots::new(
            krusty::toolchain::box_mock_jdk_rt_jar(&box_dir)
                .unwrap_or_else(|error| panic!("{error}")),
            Some(Path::new(&java_home).join("lib").join("modules")).filter(|path| path.is_file()),
        );
        let reflect = krusty::toolchain::dist_jar("kotlin-reflect.jar")
            .expect("the reference distribution's kotlin-reflect.jar is provisioned");
        Corpus {
            root,
            jdk_roots,
            reflect,
        }
    }

    fn cases(&self) -> Vec<PathBuf> {
        let mut cases = krusty::conformance::kotlin_files(&self.root);
        if let Ok(only) = std::env::var("KRUSTY_BYTECODE_TEXT_ONLY") {
            cases.retain(|case| case.to_string_lossy().contains(&only));
        }
        cases
    }

    fn key(&self, case: &Path) -> String {
        box_ratchet::corpus_key(&self.root, case)
    }

    /// The case prepared for the JVM, its stem, its modules and the classpath it compiles
    /// against, or why it is outside the JVM backend.
    fn prepare(&self, case: &Path) -> Result<Prepared, Outcome> {
        let src = std::fs::read_to_string(case)
            .unwrap_or_else(|error| panic!("read {}: {error}", case.display()));
        let src =
            krusty::conformance::prepare_test_source(&src, krusty::conformance::TestTarget::Jvm);
        if !krusty::conformance::backend_applicable(&src, krusty::conformance::BACKENDS) {
            return Err(Outcome::Excluded(
                "targeted at or muted for another backend",
            ));
        }
        let stem = case
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("File")
            .to_string();
        let modules = test_modules(&src, &stem).map_err(Outcome::Unavailable)?;
        let mut classpath = krusty::toolchain::classpath_jars_for(&src);
        if !classpath.contains(&self.reflect) {
            classpath.push(self.reflect.clone());
        }
        Ok(Prepared {
            src,
            stem,
            modules,
            classpath,
        })
    }

    fn jdk(&self, src: &str) -> BoxJdk<'_> {
        self.jdk_roots.select(src)
    }
}

struct Prepared {
    src: String,
    stem: String,
    modules: Vec<TestModule>,
    classpath: Vec<PathBuf>,
}

fn krusty_outcome(corpus: &Corpus, case: &Path) -> Outcome {
    let prepared = match corpus.prepare(case) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_box_case(
            &prepared.src,
            &prepared.stem,
            &prepared.classpath,
            corpus.jdk(&prepared.src).classpath_root(),
            &|_| {},
        )
    }));
    let compiled: CompiledClasses = match compiled {
        Ok(Some(compiled)) => compiled,
        Ok(None) => return Outcome::Fail("krusty did not compile the case".to_string()),
        Err(panic) => {
            let reason = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            return Outcome::Fail(format!("krusty panicked: {reason}"));
        }
    };
    check_case(&prepared.src, &prepared.modules, |module| {
        compiled
            .module_ranges
            .iter()
            .find(|(name, _)| name == module)
            .map(|(_, range)| {
                compiled.flat[range.clone()]
                    .iter()
                    .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                    .collect()
            })
    })
}

fn reference_outcome(corpus: &Corpus, case: &Path) -> Outcome {
    let prepared = match corpus.prepare(case) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    let reference = match box_reference_classes::reference_compile(
        &prepared.src,
        &prepared.stem,
        &prepared.classpath,
        corpus.jdk(&prepared.src),
        krusty::conformance::COROUTINE_HELPERS,
    ) {
        Ok(reference) => reference,
        Err(error) => return Outcome::Fail(format!("kotlinc did not compile the case: {error}")),
    };
    check_case(&prepared.src, &prepared.modules, |module| {
        reference.module(module).map(|classes| {
            classes
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                .collect()
        })
    })
}

fn run_corpus(
    corpus: &Corpus,
    outcome: fn(&Corpus, &Path) -> Outcome,
) -> BTreeMap<String, Outcome> {
    let mut pool = rayon::ThreadPoolBuilder::new()
        // The box lane's worker stacks: the compiler's recursive descent needs more than 8 MB in
        // the unoptimized test profile.
        .stack_size(64 * 1024 * 1024);
    if let Some(threads) = std::env::var("KRUSTY_TEST_THREADS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        pool = pool.num_threads(threads);
    }
    let pool = pool.build().expect("build the worker pool");
    let cases = corpus.cases();
    pool.install(|| {
        cases
            .par_iter()
            .map(|case| (corpus.key(case), outcome(corpus, case)))
            .collect()
    })
}

fn expectation_path(directory: &str, version: KotlinVersion) -> PathBuf {
    box_ratchet::read_path(directory, "jvm", version)
}

/// A missing list is empty: a release with nothing to excuse needs no file.
fn load_expectation(path: &Path) -> BTreeSet<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => box_ratchet::parse(&text)
            .unwrap_or_else(|error| panic!("invalid expectation {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

fn render_failures(version: KotlinVersion, failures: &BTreeSet<String>) -> String {
    let mut text = format!(
        "# Kotlin {version} {CORPUS} cases krusty does not reproduce yet, relative to that\n\
         # directory. Regenerate with {BLESS}=1 on a full local run. The list only shrinks.\n"
    );
    for failure in failures {
        text.push_str(failure);
        text.push('\n');
    }
    text
}

fn full_run() -> bool {
    std::env::var_os("KRUSTY_BYTECODE_TEXT_ONLY").is_none()
}

#[test]
fn krusty_reproduces_kotlincs_bytecode_text_corpus() {
    let corpus = Corpus::new();
    let version = krusty::kotlin_version::target();
    let outcomes = run_corpus(&corpus, krusty_outcome);
    let not_applicable = load_expectation(&expectation_path(NOT_APPLICABLE, version));
    let full = full_run();

    let mut failed = BTreeMap::new();
    let mut passed = 0;
    let mut excluded = 0;
    let mut problems = Vec::new();
    for (key, outcome) in &outcomes {
        match outcome {
            Outcome::Pass if not_applicable.contains(key) => problems.push(format!(
                "{key}: passes but is listed as not applicable; remove it from the list"
            )),
            Outcome::Pass => passed += 1,
            Outcome::Fail(_) if not_applicable.contains(key) => {}
            Outcome::Fail(reason) => {
                failed.insert(key.clone(), reason.clone());
            }
            Outcome::Excluded(_) => excluded += 1,
            Outcome::Unavailable(reason) => {
                problems.push(format!("{key}: cannot run here: {reason}"))
            }
        }
    }
    if full {
        for key in &not_applicable {
            match outcomes.get(key) {
                None => problems.push(format!(
                    "{key}: listed as not applicable but absent from the corpus"
                )),
                Some(Outcome::Excluded(reason)) => problems.push(format!(
                    "{key}: listed as not applicable but already excluded ({reason})"
                )),
                Some(_) => {}
            }
        }
    }
    let failing: BTreeSet<String> = failed.keys().cloned().collect();
    let bless = box_ratchet::bless_requested(std::env::var(BLESS).ok().as_deref())
        .unwrap_or_else(|error| panic!("{}", error.replace("BOX", "BYTECODE_TEXT")));
    if bless {
        assert!(
            std::env::var_os("CI").is_none() && full,
            "{BLESS} is refused under CI and on a partial run: expectations are blessed locally"
        );
        box_ratchet::record(
            FAILURES,
            "jvm",
            version,
            &failing,
            &render_failures(version, &failing),
        )
        .unwrap_or_else(|error| panic!("record the {version} bytecode text expectations: {error}"));
    }
    let failures_path = expectation_path(FAILURES, version);
    let expected_failures = load_expectation(&failures_path);
    for key in failing.difference(&expected_failures) {
        problems.push(format!("{key}: newly fails: {}", failed[key]));
    }
    for key in expected_failures.difference(&failing) {
        match outcomes.get(key) {
            Some(Outcome::Pass) => problems.push(format!(
                "{key}: now passes; remove it from {}",
                failures_path.display()
            )),
            Some(Outcome::Unavailable(_)) => {}
            Some(Outcome::Excluded(reason)) => problems.push(format!(
                "{key}: listed as failing but excluded ({reason}); remove it from {}",
                failures_path.display()
            )),
            Some(Outcome::Fail(_)) => {}
            None if full => problems.push(format!(
                "{key}: listed as failing but absent from the corpus"
            )),
            None => {}
        }
    }
    let applicable = outcomes.len() - excluded - not_applicable.len();
    eprintln!(
        "bytecode text corpus: {passed}/{applicable} applicable cases pass ({:.1}%); \
         {} expected failures, {} not applicable, {excluded} for other backends, of {}",
        100.0 * passed as f64 / applicable.max(1) as f64,
        failing.len(),
        not_applicable.len(),
        outcomes.len()
    );
    if !full {
        for (key, reason) in &failed {
            eprintln!("  {key}: {reason}");
        }
    }
    assert!(
        passed > 0,
        "no bytecode text case passed: check the corpus and the ASM text server"
    );
    assert!(
        problems.is_empty(),
        "{} bytecode text case(s) disagree with {}:\n  {}",
        problems.len(),
        failures_path.display(),
        problems.join("\n  ")
    );
}

/// Runs the reference kotlinc's classes through the same runner. Every case the not-applicable
/// list does not excuse must pass and every listed one must not, which checks that the runner
/// reproduces JetBrains' bytecode text test and that each listed case is a difference of
/// environment rather than a krusty gap. Ignored in ordinary runs: a cold reference cache compiles
/// the whole corpus with kotlinc.
#[test]
#[ignore = "compiles the whole bytecode text corpus with the reference kotlinc"]
fn the_reference_compiler_reproduces_every_applicable_bytecode_text_case() {
    let corpus = Corpus::new();
    let version = krusty::kotlin_version::target();
    let outcomes = run_corpus(&corpus, reference_outcome);
    let not_applicable = load_expectation(&expectation_path(NOT_APPLICABLE, version));
    let mut problems = Vec::new();
    for (key, outcome) in &outcomes {
        match outcome {
            Outcome::Pass if not_applicable.contains(key) => problems.push(format!(
                "{key}: the reference compiler reproduces it; remove it from the not-applicable list"
            )),
            Outcome::Fail(reason) if !not_applicable.contains(key) => {
                problems.push(format!("{key}: {reason}"))
            }
            Outcome::Unavailable(reason) => {
                problems.push(format!("{key}: cannot run here: {reason}"))
            }
            _ => {}
        }
    }
    assert!(
        problems.is_empty(),
        "{} bytecode text case(s) disagree with the reference compiler:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expectation_line_is_a_count_and_a_java_pattern() {
        assert_eq!(
            expected_occurrence("// 2 INVOKESTATIC kotlin/jvm/internal/Intrinsics", false),
            Some(Occurrence {
                count: 2,
                needle: "INVOKESTATIC kotlin/jvm/internal/Intrinsics".to_string(),
                inline_scopes: false,
            })
        );
        assert_eq!(
            expected_occurrence("  //0 L2I", true).map(|o| (o.count, o.needle, o.inline_scopes)),
            Some((0, "L2I".to_string(), true))
        );
        assert_eq!(
            expected_occurrence("// 1", false).map(|o| o.needle),
            Some(String::new())
        );
        assert_eq!(expected_occurrence("// FILE: a.kt", false), None);
        assert_eq!(expected_occurrence("val x = 1 // 2 nope", false), None);
    }

    #[test]
    fn an_output_file_line_names_one_class() {
        assert_eq!(output_file("// @TestKt.class:"), Some("TestKt.class"));
        assert_eq!(output_file("//@a/B.class:"), Some("a/B.class"));
        assert_eq!(output_file("// @TestKt.class"), None);
    }

    #[test]
    fn inline_scope_templates_switch_until_the_next_marker() {
        let occurrences = expected_occurrences(
            "// 1 A\n// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES\n// 2 B\n// JVM_IR_TEMPLATES\n// 3 C\n",
        );
        let flags: Vec<bool> = occurrences.iter().map(|o| o.inline_scopes).collect();
        assert_eq!(flags, [false, true, false]);
    }

    #[test]
    fn multi_file_expectations_split_at_output_files() {
        let mut occurrences = MultiFileOccurrences::default();
        occurrences
            .read("a.kt", "// 1 X\n// @A.class:\n// 2 Y\n")
            .unwrap();
        occurrences.read("b.kt", "// @B.class:\n// 0 Z\n").unwrap();
        assert_eq!(
            occurrences
                .global
                .iter()
                .map(Occurrence::render)
                .collect::<Vec<_>>(),
            ["1 X"]
        );
        assert_eq!(
            occurrences
                .by_output
                .iter()
                .map(|(file, o)| (file.as_str(), o.iter().map(Occurrence::render).collect()))
                .collect::<Vec<(&str, Vec<String>)>>(),
            [
                ("A.class", vec!["2 Y".to_string()]),
                ("B.class", vec!["0 Z".to_string()])
            ]
        );
        assert_eq!(
            occurrences.read("c.kt", "// @A.class:\n"),
            Err("c.kt: expected occurrences for output file A.class were already provided".into())
        );
    }

    #[test]
    fn created_text_heads_each_class_with_its_path_and_skips_helpers() {
        let classes = [
            OutputClass {
                path: "TestKt.class".into(),
                text: "class TestKt\n".into(),
            },
            OutputClass {
                path: "helpers/CoroutineUtilKt.class".into(),
                text: "class helpers\n".into(),
            },
        ];
        assert_eq!(
            create_text(&classes, Some(HELPERS_PREFIX)),
            "@TestKt.class\nclass TestKt\n"
        );
        assert_eq!(
            create_text(&classes, None),
            "@TestKt.class\nclass TestKt\n@helpers/CoroutineUtilKt.class\nclass helpers\n"
        );
    }
}
