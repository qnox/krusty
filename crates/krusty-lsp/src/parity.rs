//! Whole-project parity scanning: analyze every module of a real worktree and report, per module,
//! what krusty's front end says about it.
//!
//! This is the measurement half of "can krusty build project X". It deliberately reuses the LANGUAGE
//! SERVER's analysis shape rather than the batch CLI, because a repository like intellij-community is
//! mixed Java/Kotlin with no prebuilt module outputs: the only configuration in which its Kotlin
//! resolves at all is the editor one — module classpath jars, Java sources as stubs, and dependency
//! modules' sources inlined as inferred (unchecked) inputs.
//!
//! The scan is split in two so a repository with thousands of modules stays affordable:
//!   * [`plan_modules`] turns the resolved project model into one self-contained [`ModulePlan`] per
//!     module. It is pure (the file lister is injected), so the module/dependency/exclusion rules are
//!     unit-testable without a worktree.
//!   * [`run_plan`] analyzes ONE plan in the current process. The driver runs it in a child process
//!     per module, which buys parallelism across modules (the analysis graph is `Rc`-based and single
//!     threaded) and containment: a module that panics, hangs, or exhausts memory costs one module of
//!     the scan instead of the whole run.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use krusty::features::LangFeatures;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::source::{SourceInput, SourceKind};

use crate::project::model::{ProjectModel, SourceRootKind};

/// How much of a module's dependency graph is inlined as inferred sources.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyDepth {
    /// Nothing — the module is analyzed against its jar classpath alone. Fastest, and the honest
    /// measure of "compiles against binary dependencies".
    None,
    /// The module's declared dependencies (JPS export expansion already applied by the provider).
    #[default]
    Direct,
    /// The transitive closure of declared dependencies.
    All,
}

impl DependencyDepth {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "direct" => Some(Self::Direct),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Everything one module's analysis needs, resolved to absolute paths.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModulePlan {
    pub module: String,
    /// The module's own Kotlin sources — the files whose diagnostics are reported.
    pub checked: Vec<PathBuf>,
    /// Dependency Kotlin sources, analyzed for their signatures but not reported on.
    pub inferred: Vec<PathBuf>,
    /// Java sources (own and dependency) fed to the classpath as lenient stubs.
    pub java: Vec<PathBuf>,
    pub classpath: Vec<PathBuf>,
    /// `-XXLanguage:+Foo` style toggles from the module's compiler configuration.
    pub language_features: Vec<String>,
    /// Set when dependency sources were dropped to stay inside the plan's budget. A truncated plan
    /// can report unresolved references that a complete one would resolve, so the scan reports it
    /// rather than silently counting those as gaps.
    #[serde(default)]
    pub truncated: bool,
}

/// What a scan records for one module.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModuleReport {
    pub module: String,
    pub checked_files: usize,
    pub inferred_files: usize,
    pub java_files: usize,
    pub classpath_entries: usize,
    pub elapsed_ms: u64,
    /// `ok` when no error-severity diagnostic was produced.
    pub status: String,
    /// Mirrors [`ModulePlan::truncated`] into the record the harness aggregates.
    #[serde(default)]
    pub truncated: bool,
    /// Files whose text could not be read (missing, or not UTF-8). They analyzed as empty, so they
    /// contribute no diagnostics — a module reporting these is measuring less than it claims.
    #[serde(default)]
    pub unreadable_files: usize,
    #[serde(default)]
    pub unreadable_checked_files: usize,
    /// File-stem declaration names actually present in this module's checked/inferred/Java inputs.
    /// The aggregator uses this to keep its project-declaration heuristic conservative: a name that
    /// was visible to the compiler is a real compiler diagnostic, never an "unbuilt module" excuse.
    #[serde(default)]
    pub visible_declarations: Vec<String>,
    /// Java inputs existed but the lenient-stub overlay could not be built. Such a module did not
    /// analyze the source set it claims and therefore cannot count as clean.
    #[serde(default)]
    pub java_stub_failed: bool,
    pub error_count: usize,
    pub warning_count: usize,
    pub diagnostics: Vec<DiagnosticRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub file: PathBuf,
    pub line: u32,
    pub severity: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug)]
pub struct PlanOptions {
    /// Include test source roots (and test-only modules) in the scan.
    pub include_tests: bool,
    pub depth: DependencyDepth,
    /// Append the platform JDK's `lib/modules` to every module's classpath. Without it the analysis
    /// silently falls back to the bundled `.kotlin_builtins`, which reports errors on `java.*` and on
    /// mapped collection members that a real JDK resolves — pure noise in a parity scan.
    pub jdk_modules: bool,
    /// Stop adding inferred sources once their combined file count would exceed this. Each module
    /// still runs in its own bounded-lifetime child process, so a pathological file cannot retain
    /// memory in the driver or in the next module.
    pub max_inferred_files: usize,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            include_tests: false,
            depth: DependencyDepth::Direct,
            jdk_modules: true,
            max_inferred_files: 4_000,
        }
    }
}

/// Lists the source files under one source root. Injected so [`plan_modules`] is testable without a
/// worktree, and so the driver can reuse one directory walk across the modules that share a root.
pub trait SourceLister {
    fn list(&self, root: &Path) -> Vec<PathBuf>;
}

impl<F: Fn(&Path) -> Vec<PathBuf>> SourceLister for F {
    fn list(&self, root: &Path) -> Vec<PathBuf> {
        self(root)
    }
}

fn is_kotlin(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("kt")
}

fn is_java(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("java")
}

/// Build one plan per module with at least one Kotlin source of its own.
///
/// A module with no Kotlin is not "passing" — it is not a Kotlin compilation at all, and including
/// it would inflate every ratio the scan reports.
pub fn plan_modules(
    model: &ProjectModel,
    lister: &dyn SourceLister,
    options: PlanOptions,
) -> Vec<ModulePlan> {
    let jdk = options
        .jdk_modules
        .then(|| krusty::jvm::classpath::platform_jdk_modules(model.jdk_home.as_deref()))
        .flatten();
    let index_of: HashMap<&str, usize> = model
        .modules
        .iter()
        .enumerate()
        .filter_map(|(index, module)| Some((module.id.as_ref()?.as_str(), index)))
        .collect();
    // One walk per source root, shared by every module that declares it.
    let mut listed: HashMap<&Path, Vec<PathBuf>> = HashMap::new();
    for module in &model.modules {
        for root in &module.source_roots {
            listed
                .entry(root.path.as_path())
                .or_insert_with(|| lister.list(&root.path));
        }
    }
    // A source root nested inside another module's root belongs to that other module; without this
    // the outer module would claim its files and report diagnostics twice.
    let all_roots: Vec<&Path> = listed.keys().copied().collect();
    // The SAME root declared by two modules has one owner — the first module to declare it. Letting
    // both claim it checks every one of its files twice, which double-counts those errors in the
    // clusters and in the module totals.
    let mut owner: HashMap<&Path, usize> = HashMap::new();
    for (index, module) in model.modules.iter().enumerate() {
        for root in &module.source_roots {
            owner.entry(root.path.as_path()).or_insert(index);
        }
    }

    // Partition every module's files ONCE. A dependency's file list is read by each of its
    // dependents — in a repository the size of intellij-community that is tens of thousands of
    // lookups, so recomputing the nested-root filter per (module, dependency) pair is what makes
    // planning quadratic.
    let owned: Vec<(Vec<PathBuf>, Vec<PathBuf>)> = model
        .modules
        .iter()
        .enumerate()
        .map(|(index, module)| {
            let mut kotlin = BTreeSet::new();
            let mut java = BTreeSet::new();
            for root in &module.source_roots {
                if root.kind == SourceRootKind::Test && !options.include_tests {
                    continue;
                }
                if owner.get(root.path.as_path()) != Some(&index) {
                    continue;
                }
                let nested: Vec<&Path> = all_roots
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        *candidate != root.path && candidate.starts_with(&root.path)
                    })
                    .collect();
                for path in listed.get(root.path.as_path()).into_iter().flatten() {
                    if nested.iter().any(|inner| path.starts_with(inner)) {
                        continue;
                    }
                    if is_kotlin(path) {
                        kotlin.insert(path.clone());
                    } else if is_java(path) {
                        java.insert(path.clone());
                    }
                }
            }
            (
                kotlin.into_iter().collect::<Vec<_>>(),
                java.into_iter().collect::<Vec<_>>(),
            )
        })
        .collect();

    let mut plans = Vec::new();
    for (index, module) in model.modules.iter().enumerate() {
        let (checked, java) = &owned[index];
        if checked.is_empty() {
            continue;
        }
        let (checked, own_java) = (checked.clone(), java.clone());
        let mut inferred = Vec::new();
        let mut dependency_java = Vec::new();
        for dependency in dependency_indices(model, &index_of, index, options.depth) {
            let (kotlin, java) = &owned[dependency];
            inferred.extend(kotlin.iter().cloned());
            dependency_java.extend(java.iter().cloned());
        }
        let own: HashSet<&PathBuf> = checked.iter().collect();
        inferred.retain(|path| !own.contains(path));
        inferred.sort();
        inferred.dedup();
        let own_java_set: HashSet<&PathBuf> = own_java.iter().collect();
        dependency_java.retain(|path| !own_java_set.contains(path));
        dependency_java.sort();
        dependency_java.dedup();
        // ONE budget over everything a worker must read: capping only the Kotlin side let a module
        // with a wide dependency graph pull in tens of thousands of Java files, which is exactly the
        // memory blowup the cap exists to prevent.
        //
        // The module's OWN Java is outside the budget — it is what the module's own Kotlin resolves
        // against, so dropping it would manufacture unresolved references in the very files the scan
        // reports on. Only DEPENDENCY Java competes for what the inferred Kotlin leaves.
        let kept_inferred = inferred.len().min(options.max_inferred_files);
        let java_budget = options.max_inferred_files.saturating_sub(kept_inferred);
        let kept_dependency_java = dependency_java.len().min(java_budget);
        let truncated =
            kept_inferred < inferred.len() || kept_dependency_java < dependency_java.len();
        inferred.truncate(kept_inferred);
        dependency_java.truncate(kept_dependency_java);
        let mut java = own_java;
        java.extend(dependency_java);
        plans.push(ModulePlan {
            module: module
                .id
                .as_ref()
                .map_or_else(|| module.display_name.clone(), |id| id.as_str().to_string()),
            checked,
            inferred,
            java,
            classpath: module
                .classpath
                .iter()
                .cloned()
                .chain(jdk.clone())
                .collect(),
            language_features: language_features(&module.kotlinc_args),
            truncated,
        });
    }
    plans
}

/// `-XXLanguage:+Foo,-Bar` toggles, as bare feature names, from a module's compiler arguments.
fn language_features(kotlinc_args: &[String]) -> Vec<String> {
    let mut features = Vec::new();
    for argument in kotlinc_args {
        let Some(list) = argument.strip_prefix("-XXLanguage:") else {
            continue;
        };
        for entry in list.split(',') {
            if let Some(name) = entry.strip_prefix('+') {
                features.push(name.to_string());
            }
        }
    }
    features
}

fn dependency_indices(
    model: &ProjectModel,
    index_of: &HashMap<&str, usize>,
    start: usize,
    depth: DependencyDepth,
) -> Vec<usize> {
    match depth {
        DependencyDepth::None => Vec::new(),
        DependencyDepth::Direct => model.modules[start]
            .depends_on
            .iter()
            .filter_map(|id| index_of.get(id.as_str()).copied())
            .collect(),
        DependencyDepth::All => {
            let mut seen = HashSet::new();
            let mut queue = vec![start];
            let mut out = Vec::new();
            while let Some(index) = queue.pop() {
                for dependency in &model.modules[index].depends_on {
                    let Some(&next) = index_of.get(dependency.as_str()) else {
                        continue;
                    };
                    if next == start || !seen.insert(next) {
                        continue;
                    }
                    out.push(next);
                    queue.push(next);
                }
            }
            out
        }
    }
}

/// Analyze one module plan in this process.
pub fn run_plan(plan: &ModulePlan) -> ModuleReport {
    let started = std::time::Instant::now();
    // A file that cannot be read (missing, or not UTF-8 — intellij-community has both) analyzes as
    // empty. Its slot must stay, or every later file's diagnostics would shift onto the wrong path;
    // but an empty file produces no diagnostics, so silently defaulting would count it as passing.
    // Count them instead, and let the report say so.
    let read_all = |paths: &[PathBuf]| -> (Vec<String>, usize) {
        let mut unreadable = 0;
        let texts = paths
            .iter()
            .map(|path| match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(_) => {
                    unreadable += 1;
                    String::new()
                }
            })
            .collect();
        (texts, unreadable)
    };
    let (checked, unreadable_checked) = read_all(&plan.checked);
    let (inferred, unreadable_inferred) = read_all(&plan.inferred);
    let (java, unreadable_java) = read_all(&plan.java);
    let unreadable = unreadable_checked + unreadable_inferred + unreadable_java;

    let visible_declarations = plan
        .checked
        .iter()
        .chain(&plan.inferred)
        .chain(&plan.java)
        .filter_map(|path| path.file_stem()?.to_str().map(str::to_string))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let classpath = Rc::new(Classpath::new(plan.classpath.clone()));
    classpath.prepare_for_source_analysis();
    let java_stub_failed = !java.is_empty() && !set_java_stub_overlay(&classpath, &java);

    let inputs: Vec<SourceInput<'_>> = checked
        .iter()
        .chain(inferred.iter())
        .map(|text| SourceInput::new(SourceKind::Kotlin, text))
        .collect();
    let mut features = LangFeatures::new();
    for feature in &plan.language_features {
        features.enable(feature);
    }
    let platform = Box::new(JvmLibraries::new(classpath.clone()));
    let analysis = crate::compiler_analysis::analyze_source_inputs_prefix_with_features(
        &inputs,
        plan.checked.len(),
        inputs.len(),
        platform,
        &features,
    );

    let mut diagnostics = Vec::new();
    let mut error_count = 0;
    let mut warning_count = 0;
    for (index, file) in analysis.files.iter().enumerate().take(plan.checked.len()) {
        let text = &checked[index];
        for diagnostic in &file.diagnostics {
            let severity = match diagnostic.severity {
                krusty::diag::Severity::Error => {
                    error_count += 1;
                    "error"
                }
                _ => {
                    warning_count += 1;
                    "warning"
                }
            };
            if severity == "error" {
                diagnostics.push(DiagnosticRecord {
                    file: plan.checked[index].clone(),
                    line: line_of(text, diagnostic.span.lo as usize),
                    severity: severity.to_string(),
                    message: diagnostic.msg.clone(),
                });
            }
        }
    }
    ModuleReport {
        module: plan.module.clone(),
        checked_files: plan.checked.len(),
        inferred_files: plan.inferred.len(),
        java_files: plan.java.len(),
        classpath_entries: plan.classpath.len(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        status: module_status(error_count, plan.truncated, unreadable, java_stub_failed)
            .to_string(),
        truncated: plan.truncated,
        unreadable_files: unreadable,
        unreadable_checked_files: unreadable_checked,
        visible_declarations,
        java_stub_failed,
        error_count,
        warning_count,
        diagnostics,
    }
}

fn module_status(
    error_count: usize,
    truncated: bool,
    unreadable_files: usize,
    java_stub_failed: bool,
) -> &'static str {
    if unreadable_files != 0 {
        "unreadable-input"
    } else if java_stub_failed {
        "java-stub-failed"
    } else if truncated {
        "truncated"
    } else if error_count == 0 {
        "ok"
    } else {
        "errors"
    }
}

fn line_of(text: &str, offset: usize) -> u32 {
    let end = offset.min(text.len());
    1 + text.as_bytes()[..end]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32
}

/// Feed Java sources to the classpath as lenient stubs — the same overlay the analysis worker
/// installs, so a mixed Java/Kotlin module resolves its own Java types.
fn set_java_stub_overlay(classpath: &Classpath, java_sources: &[String]) -> bool {
    if java_sources.is_empty() {
        return false;
    }
    let java: Vec<(String, String)> = java_sources
        .iter()
        .map(|source| (String::new(), source.clone()))
        .collect();
    let resolve = |candidate: &str| {
        classpath
            .find_name(krusty::types::type_name(candidate))
            .is_some()
    };
    match krusty::jvm::java_stub::stub_classes(
        &java,
        krusty::jvm::java_stub::StubMode::Lenient,
        &resolve,
    ) {
        Some(stubs) => {
            classpath.set_stub_overlay(stubs);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{Module, ModuleId, ProviderKind, SourceRoot};

    struct Files(HashMap<PathBuf, Vec<PathBuf>>);

    impl SourceLister for Files {
        fn list(&self, root: &Path) -> Vec<PathBuf> {
            self.0.get(root).cloned().unwrap_or_default()
        }
    }

    fn files(entries: &[(&str, &[&str])]) -> Files {
        Files(
            entries
                .iter()
                .map(|(root, paths)| {
                    (
                        PathBuf::from(root),
                        paths.iter().map(PathBuf::from).collect(),
                    )
                })
                .collect(),
        )
    }

    fn module(id: &str, roots: Vec<SourceRoot>, deps: &[&str]) -> Module {
        let mut module = Module::new(ModuleId::raw(id), format!("/repo/{id}"));
        module.source_roots = roots;
        module.depends_on = deps.iter().map(|id| ModuleId::raw(*id)).collect();
        module
    }

    fn model(modules: Vec<Module>) -> ProjectModel {
        ProjectModel {
            root: PathBuf::from("/repo"),
            kind: ProviderKind::Jps,
            jdk_home: None,
            modules,
        }
    }

    #[test]
    fn a_module_without_kotlin_gets_no_plan() {
        let model = model(vec![module(
            "java-only",
            vec![SourceRoot::source("/repo/java-only/src")],
            &[],
        )]);
        let listed = files(&[("/repo/java-only/src", &["/repo/java-only/src/A.java"])]);
        assert!(plan_modules(&model, &listed, PlanOptions::default()).is_empty());
    }

    #[test]
    fn own_kotlin_is_checked_and_own_java_becomes_a_stub() {
        let model = model(vec![module(
            "app",
            vec![SourceRoot::source("/repo/app/src")],
            &[],
        )]);
        let listed = files(&[(
            "/repo/app/src",
            &["/repo/app/src/A.kt", "/repo/app/src/B.java"],
        )]);
        let plans = plan_modules(&model, &listed, PlanOptions::default());
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].checked, vec![PathBuf::from("/repo/app/src/A.kt")]);
        assert_eq!(plans[0].java, vec![PathBuf::from("/repo/app/src/B.java")]);
        assert!(plans[0].inferred.is_empty());
    }

    #[test]
    fn test_roots_are_excluded_unless_asked_for() {
        let model = model(vec![module(
            "app",
            vec![
                SourceRoot::source("/repo/app/src"),
                SourceRoot::test("/repo/app/test"),
            ],
            &[],
        )]);
        let listed = files(&[
            ("/repo/app/src", &["/repo/app/src/A.kt"]),
            ("/repo/app/test", &["/repo/app/test/T.kt"]),
        ]);
        let plans = plan_modules(&model, &listed, PlanOptions::default());
        assert_eq!(plans[0].checked, vec![PathBuf::from("/repo/app/src/A.kt")]);
        let with_tests = plan_modules(
            &model,
            &listed,
            PlanOptions {
                include_tests: true,
                ..PlanOptions::default()
            },
        );
        assert_eq!(with_tests[0].checked.len(), 2);
    }

    #[test]
    fn a_dependency_contributes_inferred_kotlin_and_java_stubs() {
        let model = model(vec![
            module("app", vec![SourceRoot::source("/repo/app/src")], &["lib"]),
            module("lib", vec![SourceRoot::source("/repo/lib/src")], &[]),
        ]);
        let listed = files(&[
            ("/repo/app/src", &["/repo/app/src/A.kt"]),
            (
                "/repo/lib/src",
                &["/repo/lib/src/L.kt", "/repo/lib/src/J.java"],
            ),
        ]);
        let plans = plan_modules(&model, &listed, PlanOptions::default());
        let app = plans.iter().find(|plan| plan.module == "app").unwrap();
        assert_eq!(app.checked, vec![PathBuf::from("/repo/app/src/A.kt")]);
        assert_eq!(app.inferred, vec![PathBuf::from("/repo/lib/src/L.kt")]);
        assert_eq!(app.java, vec![PathBuf::from("/repo/lib/src/J.java")]);
    }

    #[test]
    fn depth_none_drops_dependency_sources_and_depth_all_walks_the_closure() {
        let model = model(vec![
            module("app", vec![SourceRoot::source("/repo/app/src")], &["mid"]),
            module("mid", vec![SourceRoot::source("/repo/mid/src")], &["deep"]),
            module("deep", vec![SourceRoot::source("/repo/deep/src")], &[]),
        ]);
        let listed = files(&[
            ("/repo/app/src", &["/repo/app/src/A.kt"]),
            ("/repo/mid/src", &["/repo/mid/src/M.kt"]),
            ("/repo/deep/src", &["/repo/deep/src/D.kt"]),
        ]);
        let plan_with = |depth| {
            plan_modules(
                &model,
                &listed,
                PlanOptions {
                    depth,
                    ..PlanOptions::default()
                },
            )
            .into_iter()
            .find(|plan| plan.module == "app")
            .unwrap()
        };
        assert!(plan_with(DependencyDepth::None).inferred.is_empty());
        assert_eq!(plan_with(DependencyDepth::Direct).inferred.len(), 1);
        assert_eq!(plan_with(DependencyDepth::All).inferred.len(), 2);
    }

    #[test]
    fn a_dependency_cycle_terminates() {
        let model = model(vec![
            module("a", vec![SourceRoot::source("/repo/a/src")], &["b"]),
            module("b", vec![SourceRoot::source("/repo/b/src")], &["a"]),
        ]);
        let listed = files(&[
            ("/repo/a/src", &["/repo/a/src/A.kt"]),
            ("/repo/b/src", &["/repo/b/src/B.kt"]),
        ]);
        let plans = plan_modules(
            &model,
            &listed,
            PlanOptions {
                depth: DependencyDepth::All,
                ..PlanOptions::default()
            },
        );
        let a = plans.iter().find(|plan| plan.module == "a").unwrap();
        assert_eq!(a.inferred, vec![PathBuf::from("/repo/b/src/B.kt")]);
    }

    #[test]
    fn a_nested_source_root_belongs_to_the_module_that_declares_it() {
        let model = model(vec![
            module("outer", vec![SourceRoot::source("/repo/outer")], &[]),
            module("inner", vec![SourceRoot::source("/repo/outer/inner")], &[]),
        ]);
        let listed = files(&[
            (
                "/repo/outer",
                &["/repo/outer/O.kt", "/repo/outer/inner/I.kt"],
            ),
            ("/repo/outer/inner", &["/repo/outer/inner/I.kt"]),
        ]);
        let plans = plan_modules(&model, &listed, PlanOptions::default());
        let outer = plans.iter().find(|plan| plan.module == "outer").unwrap();
        assert_eq!(outer.checked, vec![PathBuf::from("/repo/outer/O.kt")]);
    }

    #[test]
    fn inferred_sources_are_capped() {
        let model = model(vec![
            module("app", vec![SourceRoot::source("/repo/app/src")], &["lib"]),
            module("lib", vec![SourceRoot::source("/repo/lib/src")], &[]),
        ]);
        let listed = files(&[
            ("/repo/app/src", &["/repo/app/src/A.kt"]),
            (
                "/repo/lib/src",
                &[
                    "/repo/lib/src/1.kt",
                    "/repo/lib/src/2.kt",
                    "/repo/lib/src/3.kt",
                ],
            ),
        ]);
        let plans = plan_modules(
            &model,
            &listed,
            PlanOptions {
                max_inferred_files: 2,
                ..PlanOptions::default()
            },
        );
        let app = plans.iter().find(|plan| plan.module == "app").unwrap();
        assert_eq!(app.inferred.len(), 2);
        assert!(app.truncated, "a capped plan must say so");
    }

    #[test]
    fn the_jdk_can_be_kept_off_the_classpath() {
        let model = model(vec![module(
            "app",
            vec![SourceRoot::source("/repo/app/src")],
            &[],
        )]);
        let listed = files(&[("/repo/app/src", &["/repo/app/src/A.kt"])]);
        let plans = plan_modules(
            &model,
            &listed,
            PlanOptions {
                jdk_modules: false,
                ..PlanOptions::default()
            },
        );
        assert!(plans[0].classpath.is_empty());
    }

    #[test]
    fn a_root_declared_by_two_modules_is_checked_once() {
        let model = model(vec![
            module("first", vec![SourceRoot::source("/repo/shared/src")], &[]),
            module("second", vec![SourceRoot::source("/repo/shared/src")], &[]),
        ]);
        let listed = files(&[("/repo/shared/src", &["/repo/shared/src/A.kt"])]);
        let plans = plan_modules(&model, &listed, PlanOptions::default());
        assert_eq!(
            plans.len(),
            1,
            "the second declaration must not check the same file again: {plans:?}"
        );
        assert_eq!(plans[0].module, "first");
    }

    /// The module names are chosen so the DEPENDENCY sorts first: a budget implemented by sorting
    /// everything together and truncating keeps whatever sorts earliest, which silently drops the
    /// module's own Java — the files its own Kotlin resolves against — while the counts still look
    /// plausible. Assert identity, not just length.
    #[test]
    fn the_budget_covers_java_but_never_drops_the_modules_own_java() {
        let model = model(vec![
            module(
                "zapp",
                vec![SourceRoot::source("/repo/zapp/src")],
                &["alib"],
            ),
            module("alib", vec![SourceRoot::source("/repo/alib/src")], &[]),
        ]);
        let listed = files(&[
            (
                "/repo/zapp/src",
                &[
                    "/repo/zapp/src/A.kt",
                    "/repo/zapp/src/Own1.java",
                    "/repo/zapp/src/Own2.java",
                ],
            ),
            (
                "/repo/alib/src",
                &[
                    "/repo/alib/src/L.kt",
                    "/repo/alib/src/J1.java",
                    "/repo/alib/src/J2.java",
                    "/repo/alib/src/J3.java",
                ],
            ),
        ]);
        let plans = plan_modules(
            &model,
            &listed,
            PlanOptions {
                max_inferred_files: 2,
                ..PlanOptions::default()
            },
        );
        let app = plans.iter().find(|plan| plan.module == "zapp").unwrap();
        assert!(app.truncated, "a plan over budget must say so");
        assert_eq!(app.inferred, vec![PathBuf::from("/repo/alib/src/L.kt")]);
        for own in ["/repo/zapp/src/Own1.java", "/repo/zapp/src/Own2.java"] {
            assert!(
                app.java.contains(&PathBuf::from(own)),
                "the module's own Java must survive the budget: {:?}",
                app.java
            );
        }
        assert!(
            app.java.len() < 5,
            "the dependency's Java is subject to the budget: {:?}",
            app.java
        );
    }

    /// The flag means "inputs were dropped", not "the plan was large". A module inside its budget
    /// must not raise a caveat the report then prints against it.
    #[test]
    fn a_plan_inside_its_budget_is_not_marked_truncated() {
        let model = model(vec![
            module("app", vec![SourceRoot::source("/repo/app/src")], &["lib"]),
            module("lib", vec![SourceRoot::source("/repo/lib/src")], &[]),
        ]);
        let listed = files(&[
            (
                "/repo/app/src",
                &[
                    "/repo/app/src/A.kt",
                    "/repo/app/src/Own1.java",
                    "/repo/app/src/Own2.java",
                ],
            ),
            ("/repo/lib/src", &["/repo/lib/src/L.kt"]),
        ]);
        let plans = plan_modules(
            &model,
            &listed,
            PlanOptions {
                max_inferred_files: 2,
                ..PlanOptions::default()
            },
        );
        let app = plans.iter().find(|plan| plan.module == "app").unwrap();
        assert!(
            !app.truncated,
            "nothing was dropped: inferred={:?} java={:?}",
            app.inferred, app.java
        );
    }

    #[test]
    fn an_unreadable_source_is_counted_not_defaulted_away() {
        let tree = crate::project::TempTree::new("parity-unreadable");
        tree.write("app/src/Ok.kt", "package app\nfun fine(): Int = 1\n");
        let plan = ModulePlan {
            module: "app".to_string(),
            checked: vec![
                tree.path("app/src/Ok.kt"),
                tree.path("app/src/DoesNotExist.kt"),
            ],
            ..ModulePlan::default()
        };
        let report = run_plan(&plan);
        assert_eq!(report.unreadable_checked_files, 1);
        assert_eq!(report.unreadable_files, 1);
        assert_eq!(report.status, "unreadable-input");
    }

    #[test]
    fn an_unreadable_dependency_source_cannot_make_a_module_look_clean() {
        let tree = crate::project::TempTree::new("parity-unreadable-dependency");
        tree.write("app/src/Ok.kt", "package app\nfun fine(): Int = 1\n");
        let plan = ModulePlan {
            module: "app".to_string(),
            checked: vec![tree.path("app/src/Ok.kt")],
            inferred: vec![tree.path("lib/src/DoesNotExist.kt")],
            ..ModulePlan::default()
        };
        let report = run_plan(&plan);
        assert_eq!(report.error_count, 0);
        assert_eq!(report.unreadable_checked_files, 0);
        assert_eq!(report.unreadable_files, 1);
        assert_eq!(report.status, "unreadable-input");
    }

    #[test]
    fn language_toggles_come_from_the_modules_compiler_arguments() {
        assert_eq!(
            language_features(&[
                "-Xjvm-default=all".to_string(),
                "-XXLanguage:+NameBasedDestructuring,-Other".to_string(),
            ]),
            vec!["NameBasedDestructuring".to_string()]
        );
    }

    /// End to end on a real (tiny) worktree: plan from a model, read the files off disk, analyze,
    /// and report. Guards the wiring the unit tests above deliberately stub out — file reading, the
    /// checked/inferred split reaching the analyzer, and diagnostics landing on the right file.
    #[test]
    fn analyzing_a_real_module_reports_its_own_errors_only() {
        let tree = crate::project::TempTree::new("parity");
        tree.write("lib/src/Lib.kt", "package lib\nfun shared(): Int = 1\n");
        tree.write(
            "app/src/Ok.kt",
            "package app\nimport lib.shared\nfun fine(): Int = shared()\n",
        );
        tree.write(
            "app/src/Bad.kt",
            "package app\nfun broken(): Int = nope()\n",
        );
        let mut app = module(
            "app",
            vec![SourceRoot::source(tree.path("app/src"))],
            &["lib"],
        );
        app.base_directory = tree.path("app");
        let mut lib = module("lib", vec![SourceRoot::source(tree.path("lib/src"))], &[]);
        lib.base_directory = tree.path("lib");
        let model = model(vec![app, lib]);
        let lister = |root: &Path| {
            let mut found = Vec::new();
            let Ok(entries) = std::fs::read_dir(root) else {
                return found;
            };
            for entry in entries.flatten() {
                found.push(entry.path());
            }
            found
        };
        let plans = plan_modules(
            &model,
            &lister,
            PlanOptions {
                jdk_modules: false,
                ..PlanOptions::default()
            },
        );
        let plan = plans.iter().find(|plan| plan.module == "app").unwrap();
        assert_eq!(plan.checked.len(), 2, "both app files are checked");
        assert_eq!(plan.inferred.len(), 1, "the dependency is inferred");
        let report = run_plan(plan);
        assert_eq!(report.checked_files, 2);
        assert_eq!(report.status, "errors");
        assert!(report.visible_declarations.contains(&"Ok".to_string()));
        assert!(report.visible_declarations.contains(&"Bad".to_string()));
        assert!(report.visible_declarations.contains(&"Lib".to_string()));
        assert!(
            report
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.file.ends_with("Bad.kt")),
            "only the broken file may be reported: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn dependency_depth_parses_its_spellings() {
        assert_eq!(DependencyDepth::parse("none"), Some(DependencyDepth::None));
        assert_eq!(
            DependencyDepth::parse("direct"),
            Some(DependencyDepth::Direct)
        );
        assert_eq!(DependencyDepth::parse("all"), Some(DependencyDepth::All));
        assert_eq!(DependencyDepth::parse("deep"), None);
    }

    #[test]
    fn line_numbers_are_one_based() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 999), 3);
        assert_eq!(
            line_of("é\nnext", 1),
            1,
            "a non-boundary byte offset must not panic"
        );
    }

    #[test]
    fn incomplete_inputs_never_count_as_clean() {
        assert_eq!(module_status(0, true, 0, false), "truncated");
        assert_eq!(module_status(0, false, 1, false), "unreadable-input");
        assert_eq!(module_status(0, false, 0, true), "java-stub-failed");
        assert_eq!(module_status(0, false, 0, false), "ok");
        assert_eq!(module_status(1, false, 0, false), "errors");
    }
}
