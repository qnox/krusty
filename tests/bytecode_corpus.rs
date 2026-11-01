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
    /// Supplied by the harness (the coroutine helpers' `support` module), not by the case.
    pub additional: bool,
}

/// One prepared case.
pub struct Case {
    pub path: PathBuf,
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
                    additional,
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
        additional: false,
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
        // A topology the box harness declines is a fixed property of the case, not of this run:
        // it fails both compilers alike, so the not-applicable list can name it with its reason.
        let modules = test_modules(&src, &stem).map_err(Outcome::Fail)?;
        let mut classpath = krusty::toolchain::classpath_jars_for(&src);
        if let Some(reflect) = &self.reflect {
            if !classpath.contains(reflect) {
                classpath.push(reflect.clone());
            }
        }
        Ok(Case {
            path: path.to_path_buf(),
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
            return Outcome::Fail(format!("krusty panicked: {reason}"));
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
             # relative to that directory. Regenerate with {}=1 on a full local run. The list only\n\
             # shrinks.\n",
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
    /// not applicable. Prints `<passed>/<applicable>`.
    pub fn assert_krusty_ratchet(&self) {
        let corpus = Corpus::new(self);
        let version = krusty::kotlin_version::target();
        let outcomes = run_corpus(&corpus, krusty_outcome);
        let not_applicable = load_expectation(&expectation_path(&self.not_applicable(), version));
        let full = corpus.only().is_none();
        let label = self.label;

        let mut failed = BTreeMap::new();
        let mut passed = 0;
        let mut excluded = 0;
        let mut listed_not_applicable = 0;
        let mut problems = Vec::new();
        for (key, outcome) in &outcomes {
            let listed = not_applicable.contains(key);
            listed_not_applicable += usize::from(listed);
            match outcome {
                Outcome::Pass if listed => problems.push(format!(
                    "{key}: passes but is listed as not applicable; remove it from the list"
                )),
                Outcome::Pass => passed += 1,
                Outcome::Fail(_) if listed => {}
                Outcome::Fail(reason) => {
                    failed.insert(key.clone(), reason.clone());
                }
                Outcome::Excluded(reason) if listed => problems.push(format!(
                    "{key}: listed as not applicable but already excluded ({reason})"
                )),
                Outcome::Excluded(_) => excluded += 1,
                Outcome::Unavailable(reason) => {
                    problems.push(format!("{key}: cannot run here: {reason}"))
                }
            }
        }
        if full {
            for key in not_applicable
                .iter()
                .filter(|key| !outcomes.contains_key(*key))
            {
                problems.push(format!(
                    "{key}: listed as not applicable but absent from the corpus"
                ));
            }
        }
        let failing: BTreeSet<String> = failed.keys().cloned().collect();
        let bless_variable = self.bless();
        let bless = box_ratchet::bless_requested(std::env::var(&bless_variable).ok().as_deref())
            .unwrap_or_else(|error| panic!("{}", error.replace("BOX", self.env)));
        if bless {
            assert!(
                std::env::var_os("CI").is_none() && full,
                "{bless_variable} is refused under CI and on a partial run: expectations are \
                 blessed locally"
            );
            box_ratchet::record(
                &self.failures(),
                "jvm",
                version,
                &failing,
                &self.render_failures(version, &failing),
            )
            .unwrap_or_else(|error| panic!("record the {version} {label} expectations: {error}"));
        }
        let failures_path = expectation_path(&self.failures(), version);
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
                Some(Outcome::Excluded(reason)) => problems.push(format!(
                    "{key}: listed as failing but excluded ({reason}); remove it from {}",
                    failures_path.display()
                )),
                Some(Outcome::Fail(_)) if not_applicable.contains(key) => problems.push(format!(
                    "{key}: listed both as failing and as not applicable"
                )),
                Some(Outcome::Fail(_) | Outcome::Unavailable(_)) => {}
                None if full => problems.push(format!(
                    "{key}: listed as failing but absent from the corpus"
                )),
                None => {}
            }
        }
        let applicable = outcomes.len() - excluded - listed_not_applicable;
        eprintln!(
            "{label} corpus: {passed}/{applicable} applicable cases pass ({:.1}%); \
             {} expected failures, {listed_not_applicable} not applicable, {excluded} for other \
             backends, of {}",
            100.0 * passed as f64 / applicable.max(1) as f64,
            failing.len(),
            outcomes.len()
        );
        if !full {
            for (key, reason) in &failed {
                eprintln!("  {key}: {reason}");
            }
        }
        assert!(
            passed > 0,
            "no {label} case passed: check the corpus and the ASM text server"
        );
        assert!(
            problems.is_empty(),
            "{} {label} case(s) disagree with {}:\n  {}",
            problems.len(),
            failures_path.display(),
            problems.join("\n  ")
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
