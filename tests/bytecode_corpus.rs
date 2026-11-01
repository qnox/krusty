//! The JetBrains bytecode corpora beside `codegen/box` (`bytecodeText`, `bytecodeListing`) as
//! ratcheted conformance lanes.
//!
//! Each corpus case is a box-style source checked against the classes a compiler emits for it. What
//! a lane checks is its own [`Suite::check`]; everything else is shared here: discovery,
//! applicability and the compilation topology (the JVM box lane's), the krusty compile, the
//! reference kotlinc compile, and the committed ratchet.
//!
//! `tests/<inventory>_expected_failures/jvm/<version>.txt` lists the cases krusty does not
//! reproduce yet; a case leaving or joining it fails until the list is updated
//! (`KRUSTY_BLESS_<ENV>_EXPECTATIONS=1` rewrites it from a full local run).
//! `tests/<inventory>_expected_not_applicable/jvm/<version>.txt` lists the cases the reference
//! compiler does not reproduce through the lane's runner, each under a comment saying why; the
//! lane's reference test holds it honest. `KRUSTY_<ENV>_ONLY=<substring>` runs a subset and reports
//! each mismatch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use krusty::conformance::{BoxJdk, BoxJdkRoots};
use krusty::kotlin_version::KotlinVersion;

use super::box_ratchet;
use super::box_reference_classes;
use super::common;
use super::kotlin_box_ir_jvm_conformance::{compile_box_case, CompiledClasses};

/// One bytecode corpus and the check its lane applies.
pub struct Suite {
    /// The corpus directory beside `codegen/box`.
    pub directory: &'static str,
    /// How reports name the lane.
    pub label: &'static str,
    /// The prefix of the lane's inventories under `tests/`.
    pub inventory: &'static str,
    /// The infix of the lane's environment knobs.
    pub env: &'static str,
    /// The suite's default `WITH_REFLECT`: kotlin-reflect joins every case's classpath.
    pub with_reflect: bool,
    /// Every expectation of a case against the classes a compiler emitted for it.
    pub check: fn(&Case, &CaseClasses<'_>) -> Outcome,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
    /// krusty panicked. A compiler defect is never an expected failure or a not-applicable case:
    /// it fails the lane whatever the inventories say, and no bless records it.
    CompilerPanic(String),
    /// Outside the JVM backend: a target or mute directive excludes it.
    Excluded(&'static str),
    /// The runner itself could not evaluate the case.
    Unavailable(String),
}

/// Why a lane could not check a case.
pub enum CheckError {
    /// The compiled classes themselves are at fault.
    Failed(String),
    /// The runner could not check them.
    Unavailable(String),
}

impl CheckError {
    pub fn into_outcome(self) -> Outcome {
        match self {
            CheckError::Failed(reason) => Outcome::Fail(reason),
            CheckError::Unavailable(reason) => Outcome::Unavailable(reason),
        }
    }
}

/// A Kotlin source file of a test module.
pub struct TestFile {
    pub name: String,
    pub content: String,
    /// Supplied by the test infrastructure (the coroutine helpers), not by the case.
    pub additional: bool,
}

/// A module of a case, named as the compilation names its classes' module.
pub struct TestModule {
    pub name: String,
    pub files: Vec<TestFile>,
}

/// One prepared case.
pub struct Case {
    pub src: String,
    pub stem: String,
    pub modules: Vec<TestModule>,
    classpath: Vec<PathBuf>,
}

/// A class a compiler emitted: its JVM internal name and its bytes.
pub type ClassFile<'a> = (&'a str, &'a [u8]);

/// The classes a compiler emitted for a case, by module, in emission order.
pub struct CaseClasses<'a> {
    modules: Vec<(&'a str, Vec<ClassFile<'a>>)>,
}

impl<'a> CaseClasses<'a> {
    pub fn module(&self, name: &str) -> Option<&[ClassFile<'a>]> {
        self.modules
            .iter()
            .find(|(module, _)| *module == name)
            .map(|(_, classes)| classes.as_slice())
    }
}

/// The modules and Kotlin files of `src`, in the topology the JVM box lane compiles it under.
fn test_modules(src: &str, stem: &str) -> Result<Vec<TestModule>, String> {
    let kotlin = |files: Vec<(String, String)>, additional: bool| -> Vec<TestFile> {
        files
            .into_iter()
            .map(|(name, content)| TestFile {
                name: format!("{name}.kt"),
                content,
                additional,
            })
            .collect()
    };
    if src.contains("// MODULE:") {
        let mut modules = krusty::conformance::split_modules(src)
            .ok_or("the case's modules are outside the box compilation model")?;
        let declared_support = modules.iter().any(|module| module.name == "support");
        if krusty::conformance::directive(src, "WITH_COROUTINES") {
            krusty::conformance::inject_support_module(
                &mut modules,
                krusty::conformance::COROUTINE_HELPERS,
            );
        }
        return Ok(krusty::conformance::module_units(&modules)
            .into_iter()
            .map(|unit| {
                let additional = unit.name == "support" && !declared_support;
                TestModule {
                    files: kotlin(unit.files, additional),
                    name: unit.name,
                }
            })
            .collect());
    }
    let mut files = if src.contains("// FILE:") || src.contains("// WITH_COROUTINES") {
        let (blocks, java) = krusty::conformance::split_files(src);
        if blocks.is_empty() && java.is_empty() {
            kotlin(vec![(stem.to_string(), src.to_string())], false)
        } else {
            kotlin(blocks, false)
        }
    } else {
        kotlin(vec![(stem.to_string(), src.to_string())], false)
    };
    if src.contains("// WITH_COROUTINES") {
        files.extend(kotlin(
            vec![(
                "CoroutineUtil".to_string(),
                krusty::conformance::COROUTINE_HELPERS.to_string(),
            )],
            true,
        ));
    }
    Ok(vec![TestModule {
        name: box_reference_classes::MAIN_MODULE.to_string(),
        files,
    }])
}

/// A suite's corpus and the inputs every case compiles against.
struct Corpus<'s> {
    suite: &'s Suite,
    root: PathBuf,
    jdk_roots: BoxJdkRoots,
    reflect: Option<PathBuf>,
}

impl<'s> Corpus<'s> {
    fn new(suite: &'s Suite) -> Self {
        let box_dir = krusty::toolchain::box_corpus_dir()
            .expect("the Kotlin box corpus is provisioned (`just box-corpus <version>`)");
        // `just box-corpus` checks the bytecode corpora out beside codegen/box.
        let root = box_dir
            .parent()
            .expect("codegen/box has a parent")
            .join(suite.directory);
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
        let reflect = suite.with_reflect.then(|| {
            krusty::toolchain::dist_jar("kotlin-reflect.jar")
                .expect("the reference distribution's kotlin-reflect.jar is provisioned")
        });
        Corpus {
            suite,
            root,
            jdk_roots,
            reflect,
        }
    }

    fn only(&self) -> Option<String> {
        std::env::var(format!("KRUSTY_{}_ONLY", self.suite.env)).ok()
    }

    fn cases(&self) -> Vec<PathBuf> {
        let mut cases = krusty::conformance::kotlin_files(&self.root);
        if let Some(only) = self.only() {
            cases.retain(|case| case.to_string_lossy().contains(&only));
        }
        cases
    }

    fn key(&self, case: &Path) -> String {
        box_ratchet::corpus_key(&self.root, case)
    }

    /// The case prepared for the JVM, or why it is outside the JVM backend.
    fn prepare(&self, path: &Path) -> Result<Case, Outcome> {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let src =
            krusty::conformance::prepare_test_source(&src, krusty::conformance::TestTarget::Jvm);
        if !krusty::conformance::backend_applicable(&src, krusty::conformance::BACKENDS) {
            return Err(Outcome::Excluded(
                "targeted at or muted for another backend",
            ));
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("File")
            .to_string();
        let modules = test_modules(&src, &stem).map_err(Outcome::Unavailable)?;
        let mut classpath = krusty::toolchain::classpath_jars_for(&src);
        if let Some(reflect) = &self.reflect {
            if !classpath.contains(reflect) {
                classpath.push(reflect.clone());
            }
        }
        Ok(Case {
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

fn krusty_outcome(corpus: &Corpus<'_>, path: &Path) -> Outcome {
    let case = match corpus.prepare(path) {
        Ok(case) => case,
        Err(outcome) => return outcome,
    };
    let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_box_case(
            &case.src,
            &case.stem,
            &case.classpath,
            corpus.jdk(&case.src).classpath_root(),
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
            return Outcome::CompilerPanic(reason);
        }
    };
    let classes = CaseClasses {
        modules: compiled
            .module_ranges
            .iter()
            .map(|(name, range)| {
                (
                    name.as_str(),
                    compiled.flat[range.clone()]
                        .iter()
                        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                        .collect(),
                )
            })
            .collect(),
    };
    (corpus.suite.check)(&case, &classes)
}

fn reference_outcome(corpus: &Corpus<'_>, path: &Path) -> Outcome {
    let case = match corpus.prepare(path) {
        Ok(case) => case,
        Err(outcome) => return outcome,
    };
    let reference = match box_reference_classes::reference_compile(
        &case.src,
        &case.stem,
        &case.classpath,
        corpus.jdk(&case.src),
        krusty::conformance::COROUTINE_HELPERS,
    ) {
        Ok(reference) => reference,
        Err(error) => return Outcome::Fail(format!("kotlinc did not compile the case: {error}")),
    };
    let classes = CaseClasses {
        modules: case
            .modules
            .iter()
            .filter_map(|module| {
                let classes = reference.module(&module.name)?;
                Some((
                    module.name.as_str(),
                    classes
                        .iter()
                        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                        .collect(),
                ))
            })
            .collect(),
    };
    (corpus.suite.check)(&case, &classes)
}

fn run_corpus(
    corpus: &Corpus<'_>,
    outcome: fn(&Corpus<'_>, &Path) -> Outcome,
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

/// One krusty run held against the inventories.
#[derive(Debug, Default)]
struct Tally {
    passed: usize,
    excluded: usize,
    listed_not_applicable: usize,
    panics: usize,
    /// Cases that fail and are not listed as not applicable, with why.
    failed: BTreeMap<String, String>,
    /// Compiler panics and cases the runner could not evaluate. No inventory excuses them, and a
    /// run that has any is incomplete, so it may not be recorded.
    fatal: Vec<String>,
    /// Disagreements with the inventories.
    problems: Vec<String>,
}

impl Tally {
    fn of(
        outcomes: &BTreeMap<String, Outcome>,
        not_applicable: &BTreeSet<String>,
        full: bool,
    ) -> Tally {
        let mut tally = Tally::default();
        for (key, outcome) in outcomes {
            let listed = not_applicable.contains(key);
            tally.listed_not_applicable += usize::from(listed);
            match outcome {
                Outcome::Pass if listed => tally.problems.push(format!(
                    "{key}: passes but is listed as not applicable; remove it from the list"
                )),
                Outcome::Pass => tally.passed += 1,
                Outcome::Fail(_) if listed => {}
                Outcome::Fail(reason) => {
                    tally.failed.insert(key.clone(), reason.clone());
                }
                Outcome::CompilerPanic(reason) => {
                    tally.panics += 1;
                    tally.fatal.push(format!("{key}: krusty panicked: {reason}"));
                }
                Outcome::Excluded(reason) if listed => tally.problems.push(format!(
                    "{key}: listed as not applicable but already excluded ({reason})"
                )),
                Outcome::Excluded(_) => tally.excluded += 1,
                Outcome::Unavailable(reason) => {
                    tally.fatal.push(format!("{key}: cannot run here: {reason}"))
                }
            }
        }
        if full {
            for key in not_applicable
                .iter()
                .filter(|key| !outcomes.contains_key(*key))
            {
                tally.problems.push(format!(
                    "{key}: listed as not applicable but absent from the corpus"
                ));
            }
        }
        tally
    }

    fn failing(&self) -> BTreeSet<String> {
        self.failed.keys().cloned().collect()
    }

    /// Only a run in which every case completed without a compiler panic is an inventory.
    fn may_record(&self) -> bool {
        self.fatal.is_empty()
    }

    fn compare_expected_failures(
        &mut self,
        outcomes: &BTreeMap<String, Outcome>,
        expected: &BTreeSet<String>,
        not_applicable: &BTreeSet<String>,
        full: bool,
    ) {
        let failing = self.failing();
        for key in failing.difference(expected) {
            let reason = &self.failed[key];
            self.problems.push(format!("{key}: newly fails: {reason}"));
        }
        for key in expected.difference(&failing) {
            match outcomes.get(key) {
                Some(Outcome::Pass) => self.problems.push(format!(
                    "{key}: now passes; remove it from the expected failures"
                )),
                Some(Outcome::Excluded(reason)) => self.problems.push(format!(
                    "{key}: listed as failing but excluded ({reason}); remove it from the \
                     expected failures"
                )),
                Some(Outcome::Fail(_)) if not_applicable.contains(key) => self
                    .problems
                    .push(format!("{key}: listed both as failing and as not applicable")),
                // Already fatal: a listed failure never excuses a panic or an unrun case.
                Some(Outcome::Fail(_) | Outcome::CompilerPanic(_) | Outcome::Unavailable(_)) => {}
                None if full => self.problems.push(format!(
                    "{key}: listed as failing but absent from the corpus"
                )),
                None => {}
            }
        }
    }
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

impl Suite {
    fn failures(&self) -> String {
        format!("{}_expected_failures", self.inventory)
    }

    fn not_applicable(&self) -> String {
        format!("{}_expected_not_applicable", self.inventory)
    }

    fn bless(&self) -> String {
        format!("KRUSTY_BLESS_{}_EXPECTATIONS", self.env)
    }

    fn render_failures(&self, version: KotlinVersion, failures: &BTreeSet<String>) -> String {
        let mut text = format!(
            "# Kotlin {version} compiler/testData/codegen/{} cases krusty does not reproduce yet,\n\
             # relative to that directory. Regenerate with {}=1\n\
             # on a full local run. The list only shrinks.\n",
            self.directory,
            self.bless()
        );
        for failure in failures {
            text.push_str(failure);
            text.push('\n');
        }
        text
    }

    /// krusty against the committed ratchet: every case passes, or fails as listed, or is listed as
    /// not applicable, and none panics. Prints `<passed>/<applicable>`.
    pub fn assert_krusty_ratchet(&self) {
        let corpus = Corpus::new(self);
        let version = krusty::kotlin_version::target();
        let outcomes = run_corpus(&corpus, krusty_outcome);
        let not_applicable = load_expectation(&expectation_path(&self.not_applicable(), version));
        let full = corpus.only().is_none();
        let label = self.label;
        let mut tally = Tally::of(&outcomes, &not_applicable, full);

        let bless_variable = self.bless();
        let bless = box_ratchet::bless_requested(std::env::var(&bless_variable).ok().as_deref())
            .unwrap_or_else(|error| panic!("{}", error.replace("BOX", self.env)));
        if bless {
            assert!(
                std::env::var_os("CI").is_none() && full,
                "{bless_variable} is refused under CI and on a partial run: expectations are \
                 blessed locally"
            );
            assert!(
                tally.may_record(),
                "{bless_variable} refused: {} case(s) panicked or could not run, so the run is no \
                 inventory:\n  {}",
                tally.fatal.len(),
                tally.fatal.join("\n  ")
            );
            box_ratchet::record(
                &self.failures(),
                "jvm",
                version,
                &tally.failing(),
                &self.render_failures(version, &tally.failing()),
            )
            .unwrap_or_else(|error| panic!("record the {version} {label} expectations: {error}"));
        }
        let failures_path = expectation_path(&self.failures(), version);
        let expected_failures = load_expectation(&failures_path);
        tally.compare_expected_failures(&outcomes, &expected_failures, &not_applicable, full);

        let applicable = outcomes.len() - tally.excluded - tally.listed_not_applicable;
        eprintln!(
            "{label} corpus: {}/{applicable} applicable cases pass ({:.1}%); \
             {} expected failures, {} not applicable, {} for other backends, {} compiler \
             panics, of {}",
            tally.passed,
            100.0 * tally.passed as f64 / applicable.max(1) as f64,
            tally.failed.len(),
            tally.listed_not_applicable,
            tally.excluded,
            tally.panics,
            outcomes.len()
        );
        if !full {
            for (key, reason) in &tally.failed {
                eprintln!("  {key}: {reason}");
            }
        }
        assert!(
            tally.passed > 0,
            "no {label} case passed: check the corpus and the ASM text server"
        );
        let problems: Vec<&String> = tally.fatal.iter().chain(&tally.problems).collect();
        assert!(
            problems.is_empty(),
            "{} {label} case(s) disagree with {}:\n  {}",
            problems.len(),
            failures_path.display(),
            problems
                .iter()
                .map(|problem| problem.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    /// The reference kotlinc's classes through the same runner: every case the not-applicable list
    /// does not excuse passes and no listed one does, which checks that the runner reproduces
    /// JetBrains' test and that each listed case is a difference of environment rather than a krusty
    /// gap.
    pub fn assert_reference_reproduces_applicable_cases(&self) {
        let corpus = Corpus::new(self);
        let version = krusty::kotlin_version::target();
        let outcomes = run_corpus(&corpus, reference_outcome);
        let not_applicable = load_expectation(&expectation_path(&self.not_applicable(), version));
        let mut problems = Vec::new();
        for (key, outcome) in &outcomes {
            match outcome {
                Outcome::Pass if not_applicable.contains(key) => problems.push(format!(
                    "{key}: the reference compiler reproduces it; remove it from the \
                     not-applicable list"
                )),
                Outcome::Fail(reason) if !not_applicable.contains(key) => {
                    problems.push(format!("{key}: {reason}"))
                }
                Outcome::Unavailable(reason) => {
                    problems.push(format!("{key}: cannot run here: {reason}"))
                }
                Outcome::CompilerPanic(reason) => {
                    problems.push(format!("{key}: panicked: {reason}"))
                }
                _ => {}
            }
        }
        assert!(
            problems.is_empty(),
            "{} {} case(s) disagree with the reference compiler:\n  {}",
            problems.len(),
            self.label,
            problems.join("\n  ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcomes(cases: Vec<(&str, Outcome)>) -> BTreeMap<String, Outcome> {
        cases
            .into_iter()
            .map(|(key, outcome)| (key.to_string(), outcome))
            .collect()
    }

    fn keys(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    #[test]
    fn a_listed_failure_does_not_excuse_a_compiler_panic() {
        let outcomes = outcomes(vec![("a.kt", Outcome::CompilerPanic("boom".to_string()))]);
        let mut tally = Tally::of(&outcomes, &keys(&[]), true);
        tally.compare_expected_failures(&outcomes, &keys(&["a.kt"]), &keys(&[]), true);
        assert_eq!(tally.fatal, ["a.kt: krusty panicked: boom"]);
        assert_eq!(tally.problems, Vec::<String>::new());
        assert!(!tally.may_record());
    }

    #[test]
    fn a_not_applicable_entry_does_not_excuse_a_compiler_panic() {
        let outcomes = outcomes(vec![("a.kt", Outcome::CompilerPanic("boom".to_string()))]);
        let tally = Tally::of(&outcomes, &keys(&["a.kt"]), true);
        assert_eq!(tally.fatal, ["a.kt: krusty panicked: boom"]);
        assert_eq!(tally.panics, 1);
        assert!(!tally.may_record());
    }

    #[test]
    fn a_run_with_an_unrun_case_is_not_recorded() {
        let outcomes = outcomes(vec![
            ("a.kt", Outcome::Pass),
            ("b.kt", Outcome::Unavailable("server died".to_string())),
        ]);
        let tally = Tally::of(&outcomes, &keys(&[]), true);
        assert_eq!(tally.fatal, ["b.kt: cannot run here: server died"]);
        assert!(!tally.may_record());
    }

    #[test]
    fn a_complete_run_records_its_ordinary_failures() {
        let outcomes = outcomes(vec![
            ("a.kt", Outcome::Pass),
            ("b.kt", Outcome::Fail("mismatch".to_string())),
            ("c.kt", Outcome::Fail("environment".to_string())),
        ]);
        let mut tally = Tally::of(&outcomes, &keys(&["c.kt"]), true);
        assert!(tally.may_record());
        assert_eq!(tally.failing(), keys(&["b.kt"]));
        tally.compare_expected_failures(&outcomes, &keys(&["b.kt"]), &keys(&["c.kt"]), true);
        assert_eq!(tally.problems, Vec::<String>::new());
        assert_eq!((tally.passed, tally.listed_not_applicable), (1, 1));
    }
}
