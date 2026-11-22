//! Compiler-plugin box corpora (`src/plugins/<name>/boxes/corpus.txt`), run by one shared path.
//!
//! A corpus is the plugin's upstream `testData` directory at the reference Kotlin tag. Every case
//! compiles with the plugin its descriptor names (the runner puts the descriptor's directive on the
//! case) together with the descriptor's support sources, against the descriptor's runtime jars and
//! Java stubs. The reference compiler runs each case first: a case it cannot compile and pass under
//! this runtime is not applicable (`expected_not_applicable`), never a krusty failure. A corpus
//! starts clean: only cases krusty passes are `included`, and each must keep passing. A case that
//! does not pass yet is simply not included; it joins the list in the change that makes it pass.
//! A case that passes without being included is an error, so the list only ever catches up.
//! Both lists live beside the descriptor.
//!
//! Env vars:
//!   KRUSTY_PLUGIN_CORPUS_DIR   root holding `<plugin>/<kotlin version>/` checkouts
//!                              (default `target/cache/plugin-corpus`; `just plugin-corpus <plugin>`)
//!   KRUSTY_PLUGIN_CORPUS_REPORT=1    print why each case that does not pass fails
//!   KRUSTY_BLESS_BOX_EXPECTATIONS=1  rewrite the plugin's inventories (full local run only)

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use krusty::conformance::plugin_corpus::{self, CaseCheck, PluginCorpus};
use rayon::prelude::*;

use super::box_jvm_compile::compile_box_case;
use super::box_ratchet::{self, Outcome};
use super::kotlin_box_ir_jvm_conformance::krusty_classes_matching_reference;
use super::{box_reference_classes, common};

const INCLUDED: &str = "included";
const NOT_APPLICABLE: &str = "expected_not_applicable";
/// The included cases whose classes are not byte-identical to the reference compiler's. Only a
/// `check = box` corpus has one: a `check = classes` case passes by being byte-identical.
const BYTE_NON_IDENTICAL: &str = "byte_non_identical";
const LANE: &str = "jvm";

fn corpus_root(corpus: &PluginCorpus, version: &str) -> PathBuf {
    let root = std::env::var_os("KRUSTY_PLUGIN_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cache/plugin-corpus")
        });
    root.join(&corpus.plugin).join(version)
}

fn boxes_dir(corpus: &PluginCorpus) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(&corpus.dir)
}

/// The `.kt` files directly under `dir`, in name order.
fn cases_in(dir: &Path) -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| {
            panic!(
                "cannot read the corpus directory {}: {error}",
                dir.display()
            )
        })
        .map(|entry| entry.expect("readable corpus entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "kt"))
        .collect();
    cases.sort();
    cases
}

/// The source a case compiles from: the descriptor's directive first, then the case, then each
/// support source as one more file of the module the case's `box()` lives in.
fn prepared_source(
    directive: &str,
    stem: &str,
    case: &str,
    support: &[(String, String)],
) -> String {
    let mut source = format!("// {directive}\n");
    if !case.contains("// FILE:") && !case.contains("// MODULE:") {
        source.push_str(&format!("// FILE: {stem}.kt\n"));
    }
    source.push_str(case);
    if !source.ends_with('\n') {
        source.push('\n');
    }
    for (name, text) in support {
        source.push_str(&format!("// FILE: {name}\n{text}\n"));
    }
    source
}

/// The corpus's Java stubs, compiled once into a class directory.
fn compile_stubs(corpus: &PluginCorpus) -> Vec<PathBuf> {
    let Some(stubs) = &corpus.stubs else {
        return Vec::new();
    };
    fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("cannot read stubs {}: {error}", dir.display()))
            .map(|entry| entry.expect("readable stub entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                sources(&path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "java")
            {
                let name = path.file_name().expect("file name").to_string_lossy();
                out.push((
                    name.into_owned(),
                    std::fs::read_to_string(&path).expect("readable stub source"),
                ));
            }
        }
    }
    let mut java = Vec::new();
    sources(&boxes_dir(corpus).join(stubs), &mut java);
    let (classes, _) = common::javac_compile(&java, &[])
        .unwrap_or_else(|| panic!("javac cannot compile the {} stubs", corpus.plugin));
    vec![classes]
}

struct Case {
    key: String,
    source: String,
    stem: String,
}

/// What running one case showed: its outcome and, for a pass, whether its classes are
/// byte-identical to the reference compiler's.
type CaseResult = Result<(Outcome, bool), String>;

fn run_case(case: &Case, check: CaseCheck, stubs: &[PathBuf]) -> CaseResult {
    let prepared = krusty::conformance::prepare_test_source(
        &case.source,
        krusty::conformance::TestTarget::Jvm,
    );
    match check {
        CaseCheck::Box => run_box_case(case, &prepared, stubs),
        CaseCheck::Classes => run_classes_case(case, &prepared, stubs),
    }
}

/// The case compiles with krusty and returns `"OK"` from `box()`.
fn run_box_case(case: &Case, prepared: &str, stubs: &[PathBuf]) -> CaseResult {
    let Ok(reference) =
        box_reference_classes::checked_prepared_box_with(prepared, &case.stem, stubs)
    else {
        return Ok((Outcome::NotApplicable, true));
    };
    let mut classpath = common::classpath_jars_for(prepared);
    classpath.extend_from_slice(stubs);
    let jdk = common::jdk_modules();
    let compiled = match compile_box_case(
        prepared,
        &case.stem,
        &classpath,
        Some(jdk.as_path()),
        &|_| {},
    ) {
        Ok(compiled) => compiled,
        Err(failure) => return Err(format!("{failure:?}")),
    };
    let Some(box_class) = compiled.box_class() else {
        return Err("no box() in the final module".to_string());
    };
    match common::run_box(&compiled.flat, &box_class, &classpath).as_deref() {
        Some("OK") => {
            let identical =
                krusty_classes_matching_reference(prepared, &case.stem, &classpath, &reference)
                    .is_ok();
            Ok((Outcome::Pass, identical))
        }
        other => Err(format!("box() returned {other:?}")),
    }
}

/// The case compiles to exactly the classes the reference compiler emits. A case the reference
/// itself rejects is not applicable.
fn run_classes_case(case: &Case, prepared: &str, stubs: &[PathBuf]) -> CaseResult {
    let Ok(reference) = box_reference_classes::reference_classes_with(prepared, &case.stem, stubs)
    else {
        return Ok((Outcome::NotApplicable, true));
    };
    let mut classpath = common::classpath_jars_for(prepared);
    classpath.extend_from_slice(stubs);
    krusty_classes_matching_reference(prepared, &case.stem, &classpath, &reference)
        .map(|_| (Outcome::Pass, true))
}

fn render(
    corpus: &PluginCorpus,
    version: krusty::kotlin_version::KotlinVersion,
    expected: &str,
    paths: &BTreeSet<String>,
) -> String {
    let mut text = format!(
        "# Kotlin {version} {} box cases that {expected} on {LANE}, relative to\n\
         # the upstream corpus directory. Later releases with no file of their own read this one.\n\
         # Regenerate with KRUSTY_BLESS_BOX_EXPECTATIONS=1 on a full local run.\n",
        corpus.plugin
    );
    for path in paths {
        text.push_str(path);
        text.push('\n');
    }
    text
}

fn run_corpus(corpus: &PluginCorpus) {
    let version = krusty::kotlin_version::environment_release();
    if !corpus.kotlin.contains(version) {
        eprintln!(
            "{} corpus: skipped, it covers Kotlin {} and this run is {version}",
            corpus.plugin, corpus.kotlin
        );
        return;
    }
    let directive = corpus
        .directive
        .as_deref()
        .expect("a runnable corpus names its directive");
    let root = corpus_root(corpus, &version.to_string());
    assert!(
        root.is_dir(),
        "the {} corpus is not provisioned at {}: run `just plugin-corpus {} {version}`",
        corpus.plugin,
        root.display(),
        corpus.plugin
    );
    let support: Vec<(String, String)> = corpus
        .support
        .iter()
        .map(|path| {
            let file = root.join(path);
            let name = file
                .file_name()
                .expect("support file name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&file).unwrap_or_else(|error| {
                panic!("cannot read support source {}: {error}", file.display())
            });
            (name, text)
        })
        .collect();
    let mut cases = Vec::new();
    for upstream in &corpus.upstream {
        let dir = root.join(upstream);
        for file in cases_in(&dir) {
            let stem = file
                .file_stem()
                .expect("case stem")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()));
            cases.push(Case {
                key: file
                    .strip_prefix(&dir)
                    .expect("case under its directory")
                    .to_string_lossy()
                    .into_owned(),
                source: prepared_source(directive, &stem, &text, &support),
                stem,
            });
        }
    }
    assert!(
        !cases.is_empty(),
        "the {} corpus has no cases under {}",
        corpus.plugin,
        root.display()
    );
    let stubs = compile_stubs(corpus);
    let results: Vec<(String, CaseResult)> = cases
        .par_iter()
        .map(|case| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_case(case, corpus.check, &stubs)
            }))
            .unwrap_or_else(|_| Err("compiler panic".to_string()));
            (case.key.clone(), result)
        })
        .collect();
    let panics: Vec<&str> = results
        .iter()
        .filter(|(_, result)| {
            result
                .as_ref()
                .is_err_and(|reason| reason == "compiler panic")
        })
        .map(|(key, _)| key.as_str())
        .collect();
    assert!(panics.is_empty(), "the compiler panicked on: {panics:?}");
    let outcomes: BTreeMap<String, Outcome> = results
        .iter()
        .map(|(key, result)| {
            (
                key.clone(),
                result
                    .as_ref()
                    .map_or(Outcome::Fail, |(outcome, _)| *outcome),
            )
        })
        .collect();
    let reasons: BTreeMap<&str, &str> = results
        .iter()
        .filter_map(|(key, result)| {
            result
                .as_ref()
                .err()
                .map(|reason| (key.as_str(), reason.as_str()))
        })
        .collect();
    let passing: BTreeSet<String> = outcomes
        .iter()
        .filter(|(_, outcome)| **outcome == Outcome::Pass)
        .map(|(key, _)| key.clone())
        .collect();
    let non_identical: BTreeSet<String> = results
        .iter()
        .filter(|(_, result)| matches!(result, Ok((Outcome::Pass, false))))
        .map(|(key, _)| key.clone())
        .collect();
    let not_applicable: BTreeSet<String> = outcomes
        .iter()
        .filter(|(_, outcome)| **outcome == Outcome::NotApplicable)
        .map(|(key, _)| key.clone())
        .collect();
    if box_ratchet::bless_requested(
        std::env::var("KRUSTY_BLESS_BOX_EXPECTATIONS")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|error| panic!("{error}"))
    {
        assert!(
            std::env::var_os("CI").is_none(),
            "blessing is refused under CI"
        );
        let dir = boxes_dir(corpus);
        // Blessing records what passes now; a case that was included and no longer passes is a
        // regression to fix, never one to drop from the list.
        let included_before = box_ratchet::load_path(
            &box_ratchet::read_path_in(&dir, INCLUDED, LANE, version),
            false,
        );
        let regressed: Vec<&String> = included_before
            .iter()
            .filter(|key| !passing.contains(*key))
            .collect();
        assert!(
            regressed.is_empty(),
            "{} corpus: included cases no longer pass, so blessing is refused: {regressed:?}",
            corpus.plugin
        );
        let mut inventories = vec![
            (INCLUDED, &passing, "pass and are part of the corpus"),
            (NOT_APPLICABLE, &not_applicable, "be not-applicable"),
        ];
        if corpus.check == CaseCheck::Box {
            inventories.push((
                BYTE_NON_IDENTICAL,
                &non_identical,
                "pass with classes that are not byte-identical to the reference compiler's",
            ));
        }
        for (name, paths, what) in inventories {
            box_ratchet::record_in(
                &dir,
                name,
                LANE,
                version,
                paths,
                &render(corpus, version, what, paths),
            )
            .unwrap_or_else(|error| panic!("cannot record {name}: {error}"));
        }
        eprintln!(
            "{} corpus: recorded {} included and {} not-applicable of {} upstream cases",
            corpus.plugin,
            passing.len(),
            not_applicable.len(),
            outcomes.len()
        );
        return;
    }
    for line in not_passing_report(
        &corpus.plugin,
        &reasons,
        std::env::var_os("KRUSTY_PLUGIN_CORPUS_REPORT").is_some(),
    ) {
        eprintln!("{line}");
    }
    let root = boxes_dir(corpus);
    let included = box_ratchet::load_path(
        &box_ratchet::read_path_in(&root, INCLUDED, LANE, version),
        true,
    );
    let expected_not_applicable = box_ratchet::load_path(
        &box_ratchet::read_path_in(&root, NOT_APPLICABLE, LANE, version),
        true,
    );
    let mut problems = Vec::new();
    if corpus.check == CaseCheck::Box {
        let expected = box_ratchet::load_path(
            &box_ratchet::read_path_in(&root, BYTE_NON_IDENTICAL, LANE, version),
            true,
        );
        for key in non_identical.difference(&expected) {
            problems.push(format!(
                "{key}: passes with classes that differ from the reference compiler's, and is not in {BYTE_NON_IDENTICAL}"
            ));
        }
        for key in expected.difference(&non_identical) {
            problems.push(format!(
                "{key}: listed in {BYTE_NON_IDENTICAL} but byte-identical or not passing; remove it"
            ));
        }
    }
    for key in &included {
        match outcomes.get(key) {
            Some(Outcome::Pass) => {}
            Some(outcome) => problems.push(format!(
                "{key}: included in the corpus but {outcome:?}: {}",
                reasons.get(key.as_str()).copied().unwrap_or("")
            )),
            None => problems.push(format!("{key}: included but not in the upstream corpus")),
        }
    }
    for key in passing.difference(&included) {
        problems.push(format!(
            "{key}: passes but is not included; add it to {INCLUDED}"
        ));
    }
    for key in not_applicable.symmetric_difference(&expected_not_applicable) {
        problems.push(format!(
            "{key}: not-applicable {} the reference run",
            if not_applicable.contains(key) {
                "by"
            } else {
                "listed but not by"
            }
        ));
    }
    assert!(
        problems.is_empty(),
        "{} corpus disagrees with its inventories under {}/{{{INCLUDED},{NOT_APPLICABLE},{BYTE_NON_IDENTICAL}}}:\n{}",
        corpus.plugin,
        corpus.dir,
        problems.join("\n")
    );
    assert!(
        !included.is_empty(),
        "no {} case is included: check the runtime and the plugin jar",
        corpus.plugin
    );
    eprintln!(
        "{} corpus: {} cases included and passing, {} not applicable, {} not yet included of {}",
        corpus.plugin,
        included.len(),
        not_applicable.len(),
        outcomes.len() - included.len() - not_applicable.len(),
        outcomes.len()
    );
}

/// Every compiler plugin's box corpus matches its committed inventories.
/// The lines `KRUSTY_PLUGIN_CORPUS_REPORT` prints: one per case that does not pass, with its reason;
/// nothing unless the report is enabled.
fn not_passing_report(plugin: &str, reasons: &BTreeMap<&str, &str>, enabled: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    reasons
        .iter()
        .map(|(key, reason)| format!("{plugin} corpus: not passing: {key}: {reason}"))
        .collect()
}

#[test]
fn the_report_names_each_non_passing_case_with_its_reason_only_when_enabled() {
    let reasons = BTreeMap::from([("a.kt", "box() returned None"), ("b.kt", "no box()")]);
    assert_eq!(
        not_passing_report("demo", &reasons, true),
        [
            "demo corpus: not passing: a.kt: box() returned None",
            "demo corpus: not passing: b.kt: no box()",
        ]
    );
    assert_eq!(
        not_passing_report("demo", &reasons, false),
        Vec::<String>::new()
    );
}

#[test]
fn plugin_box_corpora_match_their_inventories() {
    let runnable: Vec<&PluginCorpus> = plugin_corpus::all()
        .iter()
        .filter(|corpus| corpus.directive.is_some() && !corpus.upstream.is_empty())
        .collect();
    assert!(!runnable.is_empty(), "no plugin corpus is runnable");
    for corpus in runnable {
        run_corpus(corpus);
    }
}

#[test]
fn a_case_gets_the_directive_and_its_support_sources() {
    let support = [("lib.kt".to_string(), "fun helper() {}".to_string())];
    assert_eq!(
        prepared_source("WITH_X", "simple", "// WITH_STDLIB\nfun box() = \"OK\"", &support),
        "// WITH_X\n// FILE: simple.kt\n// WITH_STDLIB\nfun box() = \"OK\"\n// FILE: lib.kt\nfun helper() {}\n"
    );
    assert_eq!(
        prepared_source(
            "WITH_X",
            "multi",
            "// FILE: a.kt\nfun box() = \"OK\"\n",
            &[]
        ),
        "// WITH_X\n// FILE: a.kt\nfun box() = \"OK\"\n"
    );
}

#[test]
fn a_plugin_corpus_lists_its_inclusions_beside_its_descriptor() {
    let corpus = plugin_corpus::named("parcelize");
    let version = krusty::kotlin_version::KotlinVersion::V2_4_21;
    assert!(
        box_ratchet::read_path_in(&boxes_dir(corpus), INCLUDED, LANE, version).starts_with(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/parcelize/boxes/included/jvm")
        )
    );
}
