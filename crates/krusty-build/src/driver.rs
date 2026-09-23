//! The build driver: plan, look up, compile, store, materialize.
//!
//! Sequential on purpose. The compiler is not `Send` — its state deliberately holds `Rc`/`RefCell`
//! (hence the `stacker` dependency) and interning leaks, which is why the language server restarts
//! its worker every 64 analyses. So parallelism has to be process-per-module or a pool of restarted
//! workers, and that is a separate change with its own failure modes (crash isolation, diagnostic
//! interleaving, classpath warmth). Getting the sequential driver correct first gives the parallel
//! one an oracle: the same graph must produce the same artifacts either way.
//!
//! # What a cache hit must still do
//!
//! Reuse is not just "skip the compile". A dependent reads its dependency's output from disk, so a
//! hit still has to **materialize** the cached artifacts into the module's output directory. A
//! driver that skipped that would produce a build whose second run is broken in a way the first run
//! is not — the worst possible shape for a caching bug.
//!
//! # Ordering
//!
//! A module is built only after every dependency is built, so its dependencies' ABI fingerprints
//! are known and can enter its cache key. That is what makes avoidance transitive: a body-only edit
//! deep in the graph changes that module's key and nothing else's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::abi::AbiFingerprint;
use crate::cache::{CacheKey, CacheKeyInputs};
use crate::digest::{digest_bytes, Hasher};
use crate::graph::{GraphError, ModuleGraph};
use crate::model::{Module, ModuleId, ModuleOutput};
use crate::store::{ArtifactStore, CachedModule, MissReason};

/// The dependency identity a compiler environment can soundly publish.
///
/// The reduced class-header model deliberately carries no method bodies, which is what makes
/// avoidance possible — but a complete semantic ABI must also carry facts outside those headers:
///
/// * **`inline` functions.** On this target an inline body is bytecode spliced into the CALL SITE
///   (`src/jvm/inline.rs`), so editing one changes what every dependent compiles to while changing
///   no signature.
/// * **`contract { … }` blocks.** They live syntactically inside a body but drive callers'
///   smart-cast analysis (`src/contracts.rs`).
/// * **Non-`SOURCE` annotations and module metadata.** Their payloads affect dependent frontend
///   decisions even when a class/member descriptor is unchanged.
///
/// An environment that cannot prove every such observable is represented must not have dependents
/// keyed on the reduced signature fingerprint: it would not move, and the dependent could take a
/// stale cache hit with wrong compiled behavior. [`Driver`] therefore publishes a whole-output
/// digest for an incomplete module, so any change invalidates dependents. That is conservative but
/// sound; the concrete JVM adapter uses this path until its semantic ABI is complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishedAbi {
    /// A semantic identity supplied by an environment that owns and proves its complete ABI model.
    Exact(AbiFingerprint),
    /// No complete ABI model is available; hash every emitted artifact instead.
    WholeOutput { reason: String },
}

/// What a compiler hands back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledModule {
    /// Emitted artifacts as `(target-relative path, bytes)`, in emission order.
    pub artifacts: Vec<(String, Vec<u8>)>,
    pub published_abi: PublishedAbi,
}

/// Everything the driver needs from the outside world, behind one trait so the build logic can be
/// tested without a compiler, a JDK, or a filesystem full of jars.
pub trait BuildEnvironment {
    /// The module's cache-key inputs EXCEPT `dependency_abis`, which only the driver knows because
    /// it depends on build order. `dependency_outputs` are supplied only so an environment can
    /// inventory sidecar inputs selected by the exact compiler classpath; the dependency output
    /// trees themselves remain represented by their published ABI identities.
    fn base_inputs(
        &self,
        module: &Module,
        dependency_outputs: &[PathBuf],
    ) -> Result<CacheKeyInputs, String>;

    /// Why this environment cannot faithfully build `module`, if it cannot.
    ///
    /// The model can express more than any given environment implements. Silently ignoring a field
    /// it promises — a per-module JDK, resources, annotation processors, a jar output — produces a
    /// module that is quietly wrong and then caches it under a key that looks valid. Refusing is
    /// the same contract as a module with Java sources: loud, and never a short artifact.
    fn unsupported(&self, _module: &Module) -> Option<String> {
        None
    }

    /// Compile one module. Called only on a cache miss.
    ///
    /// `dependency_outputs` are every already-materialized output of this module's dependencies,
    /// in dependency order and then declaration order, to be placed on the compile classpath. They
    /// are deliberately NOT part of
    /// [`Self::base_inputs`]: a dependency's output bytes change on any body edit, so digesting
    /// them into the key would make every dependent rebuild on every edit and defeat avoidance
    /// entirely. A dependency's contribution to the key is its ABI fingerprint, which the driver
    /// supplies separately.
    fn compile(
        &mut self,
        module: &Module,
        dependency_outputs: &[PathBuf],
    ) -> Result<CompiledModule, String>;
}

/// What happened to one module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Served from the cache and materialized into the output directory. No compiler ran.
    CacheHit { key: CacheKey, abi: AbiFingerprint },
    /// Compiled, stored, and materialized.
    Compiled { key: CacheKey, abi: AbiFingerprint },
    /// krusty cannot compile this module at all — it contains Java sources and there is no Java
    /// frontend. Refused loudly rather than emitting the Kotlin half as a short jar.
    Refused { reason: String },
    /// The compiler or the environment reported an error.
    Failed { message: String },
    /// A dependency did not produce output, so this module was never attempted. Its own cache entry
    /// is neither read nor written: a key computed from an absent dependency ABI would be a lie.
    Blocked { on: ModuleId },
}

impl Outcome {
    pub fn abi(&self) -> Option<AbiFingerprint> {
        match self {
            Self::CacheHit { abi, .. } | Self::Compiled { abi, .. } => Some(*abi),
            _ => None,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::CacheHit { .. } | Self::Compiled { .. })
    }

    fn label(&self) -> &'static str {
        match self {
            Self::CacheHit { .. } => "cached",
            Self::Compiled { .. } => "compiled",
            Self::Refused { .. } => "refused",
            Self::Failed { .. } => "failed",
            Self::Blocked { .. } => "blocked",
        }
    }
}

/// One build's results, in build order.
#[derive(Clone, Debug, Default)]
pub struct BuildReport {
    pub outcomes: Vec<(ModuleId, Outcome)>,
}

impl BuildReport {
    pub fn get(&self, id: &ModuleId) -> Option<&Outcome> {
        self.outcomes
            .iter()
            .find(|(module, _)| module == id)
            .map(|(_, outcome)| outcome)
    }

    pub fn compiled(&self) -> Vec<&ModuleId> {
        self.with_label("compiled")
    }

    pub fn cache_hits(&self) -> Vec<&ModuleId> {
        self.with_label("cached")
    }

    fn with_label(&self, label: &str) -> Vec<&ModuleId> {
        self.outcomes
            .iter()
            .filter(|(_, outcome)| outcome.label() == label)
            .map(|(id, _)| id)
            .collect()
    }

    /// Every module produced output.
    pub fn is_success(&self) -> bool {
        self.outcomes.iter().all(|(_, outcome)| outcome.is_ok())
    }

    /// One line per module, in build order — what a `krusty build` run would print.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (id, outcome) in &self.outcomes {
            match outcome {
                Outcome::CacheHit { key, .. } => out.push_str(&format!("cached    {id} ({key})\n")),
                Outcome::Compiled { key, .. } => out.push_str(&format!("compiled  {id} ({key})\n")),
                Outcome::Refused { reason } => out.push_str(&format!("refused   {id}: {reason}\n")),
                Outcome::Failed { message } => {
                    out.push_str(&format!("failed    {id}: {message}\n"))
                }
                Outcome::Blocked { on } => out.push_str(&format!("blocked   {id} (needs {on})\n")),
            }
        }
        out
    }
}

pub struct Driver<E: BuildEnvironment> {
    environment: E,
    store: ArtifactStore,
}

impl<E: BuildEnvironment> Driver<E> {
    pub fn new(environment: E, store: ArtifactStore) -> Self {
        Self { environment, store }
    }

    pub fn environment(&self) -> &E {
        &self.environment
    }

    /// Build every module in dependency order.
    ///
    /// `Err` is reserved for a graph that cannot be planned at all (a cycle, an unknown dependency).
    /// A module that fails to compile is a [`Outcome::Failed`] in the report, not an `Err`: the
    /// build continues so one run reports every independent failure rather than only the first.
    pub fn build(&mut self, graph: &ModuleGraph) -> Result<BuildReport, GraphError> {
        let order = graph.build_order()?;
        let mut report = BuildReport::default();
        let mut built: BTreeMap<ModuleId, AbiFingerprint> = BTreeMap::new();

        for id in order {
            let Some(module) = graph.get(&id) else {
                continue;
            };
            let outcome = self.build_one(module, &id, graph, &built);
            if let Some(abi) = outcome.abi() {
                built.insert(id.clone(), abi);
            }
            report.outcomes.push((id, outcome));
        }
        Ok(report)
    }

    fn build_one(
        &mut self,
        module: &Module,
        id: &ModuleId,
        graph: &ModuleGraph,
        built: &BTreeMap<ModuleId, AbiFingerprint>,
    ) -> Outcome {
        // A module krusty cannot compile is refused before anything else: emitting its Kotlin half
        // would produce output that is wrong in a way nothing downstream can detect.
        if !module.is_krusty_compilable() {
            return Outcome::Refused {
                reason: format!(
                    "{} Java source(s); krusty has no Java frontend",
                    module.java_sources.len()
                ),
            };
        }
        // Anything the environment cannot honor faithfully is refused BEFORE a key is computed, so
        // an unfaithful build can never be cached.
        if let Some(reason) = self.environment.unsupported(module) {
            return Outcome::Refused { reason };
        }

        // Every dependency must have produced an ABI, or this module's key would be computed from
        // incomplete inputs — a key that could collide with a later, correct build.
        let mut dependency_abis = Vec::new();
        for dependency in graph.direct_dependencies(id) {
            match built.get(dependency) {
                Some(abi) => dependency_abis.push((dependency.clone(), *abi)),
                None => {
                    return Outcome::Blocked {
                        on: dependency.clone(),
                    }
                }
            }
        }

        // Every declared output of every dependency, for the compile classpath only — never for
        // the key. Selecting only the first output silently compiles against a shorter dependency
        // than the module model declared.
        let dependency_outputs: Vec<PathBuf> = graph
            .direct_dependencies(id)
            .iter()
            .filter_map(|dependency| graph.get(dependency))
            .flat_map(|dependency| dependency.outputs.iter())
            .map(|output| output.path().to_path_buf())
            .collect();

        let mut inputs = match self.environment.base_inputs(module, &dependency_outputs) {
            Ok(inputs) => inputs,
            Err(message) => return Outcome::Failed { message },
        };
        inputs.dependency_abis = dependency_abis;
        let key = inputs.key();

        match self.store.get(key) {
            Ok(Ok(cached)) => match materialize(&cached.artifacts, module) {
                Ok(()) => Outcome::CacheHit {
                    key,
                    abi: cached.abi,
                },
                Err(message) => Outcome::Failed { message },
            },
            Ok(Err(MissReason::Absent)) => self.compile_and_store(module, key, &dependency_outputs),
            Ok(Err(_damaged)) => {
                // A damaged entry degrades to a recompile rather than a failed build. The store
                // treats every damage mode as a miss for exactly this reason.
                self.compile_and_store(module, key, &dependency_outputs)
            }
            Err(error) => Outcome::Failed {
                message: format!("cache read failed: {error}"),
            },
        }
    }

    fn compile_and_store(
        &mut self,
        module: &Module,
        key: CacheKey,
        dependency_outputs: &[PathBuf],
    ) -> Outcome {
        let compiled = match self.environment.compile(module, dependency_outputs) {
            Ok(compiled) => compiled,
            Err(message) => return Outcome::Failed { message },
        };

        let abi = published_abi(&compiled.artifacts, &compiled.published_abi);

        let entry = CachedModule {
            artifacts: compiled.artifacts,
            abi,
        };
        if let Err(error) = self.store.put(key, &entry) {
            return Outcome::Failed {
                message: format!("cache write failed: {error}"),
            };
        }
        match materialize(&entry.artifacts, module) {
            Ok(()) => Outcome::Compiled { key, abi },
            Err(message) => Outcome::Failed { message },
        }
    }
}

/// The ABI value a module publishes to its dependents.
///
/// An exact identity is accepted only from the environment that owns its semantic ABI proof. A
/// whole-output publication is computed here so an incomplete adapter cannot accidentally omit an
/// artifact. The two forms are domain-separated.
fn published_abi(artifacts: &[(String, Vec<u8>)], publication: &PublishedAbi) -> AbiFingerprint {
    if let PublishedAbi::WholeOutput { reason } = publication {
        let mut hasher = Hasher::new();
        hasher.text("abi-kind", "whole-output");
        hasher.text("reason", reason);
        let mut ordered: Vec<&(String, Vec<u8>)> = artifacts.iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(&b.0));
        hasher.count("artifacts", ordered.len());
        for (path, bytes) in ordered {
            hasher.text("path", path);
            hasher.nested("content", digest_bytes(bytes));
        }
        return AbiFingerprint::from_digest(hasher.finish());
    }
    let PublishedAbi::Exact(abi) = publication else {
        unreachable!()
    };
    let mut hasher = Hasher::new();
    hasher.text("abi-kind", "exact");
    hasher.nested("semantic-abi", abi.digest());
    AbiFingerprint::from_digest(hasher.finish())
}

/// Replace each output directory's contents with `artifacts`.
///
/// Runs on a cache hit as well as a compile: reuse means "the same files are on disk", not "the
/// compiler did not run".
///
/// This REPLACES rather than overlays. Writing the current artifacts over whatever was there before
/// leaves files behind that the module no longer produces — delete or rename a class and its stale
/// `.class` stays on disk, visible to every dependent, on both the rebuild and the cache-hit path.
/// A build whose output depends on what happened to be there before is not reproducible.
///
/// The swap goes through a staging directory so the window in which the output is incomplete is a
/// rename rather than the whole write. [`CompiledModule`] represents one artifact tree, so an
/// environment admitting several outputs explicitly requests that same tree be mirrored to each;
/// an environment whose outputs are independent trees must refuse that module instead.
fn materialize(artifacts: &[(String, Vec<u8>)], module: &Module) -> Result<(), String> {
    for output in &module.outputs {
        let ModuleOutput::ClassDirectory(directory) = output else {
            // A jar output is refused up front (see `BuildEnvironment::unsupported`); reaching here
            // would mean a module was admitted that cannot be materialized.
            return Err(format!(
                "cannot materialize into {}: only a class directory is supported",
                output.path().display()
            ));
        };
        replace_tree(directory, artifacts)?;
    }
    Ok(())
}

/// Build the tree beside the target, then swap it in and drop the old one.
fn replace_tree(directory: &Path, artifacts: &[(String, Vec<u8>)]) -> Result<(), String> {
    let parent = directory.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;

    let suffix = format!("{}-{}", std::process::id(), staging_counter());
    let staging = parent.join(format!(
        ".{}.staging-{suffix}",
        directory
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("out")
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("cannot create {}: {error}", staging.display()))?;
    write_tree(&staging, artifacts)?;

    let retired = parent.join(format!(
        ".{}.replaced-{suffix}",
        directory
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("out")
    ));
    let had_previous = directory.exists();
    if had_previous {
        std::fs::rename(directory, &retired)
            .map_err(|error| format!("cannot set aside {}: {error}", directory.display()))?;
    }
    match std::fs::rename(&staging, directory) {
        Ok(()) => {
            if had_previous {
                let _ = std::fs::remove_dir_all(&retired);
            }
            Ok(())
        }
        Err(error) => {
            // Put the previous output back rather than leaving nothing in place.
            if had_previous {
                let _ = std::fs::rename(&retired, directory);
            }
            let _ = std::fs::remove_dir_all(&staging);
            Err(format!("cannot publish {}: {error}", directory.display()))
        }
    }
}

fn staging_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn write_tree(directory: &Path, artifacts: &[(String, Vec<u8>)]) -> Result<(), String> {
    for (path, bytes) in artifacts {
        let destination: PathBuf = directory.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        std::fs::write(&destination, bytes)
            .map_err(|error| format!("cannot write {}: {error}", destination.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::FileDigest;
    use crate::model::ModuleOutput;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "krusty-build-driver-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fake compiler. It emits no real class files, so the ABI is derived from a caller-supplied
    /// "signature" string rather than parsed bytes — which lets a test vary BODY and SIGNATURE
    /// independently, the distinction the whole cache design rests on.
    struct FakeEnvironment {
        /// module id -> (body marker, signature marker)
        sources: BTreeMap<String, (String, String)>,
        compiles: BTreeMap<String, usize>,
        /// Force conservative whole-output publication instead of the fake semantic ABI.
        whole_output_reason: Option<String>,
        /// module id -> reason this environment cannot honor it.
        refuse: BTreeMap<String, String>,
        /// Artifact names to emit, so a test can drop one and check stale output is removed.
        artifact_names: Option<Vec<String>>,
        /// Dependency outputs observed by each compilation.
        dependency_outputs: BTreeMap<String, Vec<PathBuf>>,
    }

    impl FakeEnvironment {
        fn new(_output_root: &Path) -> Self {
            Self {
                sources: BTreeMap::new(),
                compiles: BTreeMap::new(),
                whole_output_reason: None,
                refuse: BTreeMap::new(),
                artifact_names: None,
                dependency_outputs: BTreeMap::new(),
            }
        }

        fn set(&mut self, id: &str, body: &str, signature: &str) {
            self.sources
                .insert(id.into(), (body.into(), signature.into()));
        }

        fn compile_count(&self, id: &str) -> usize {
            self.compiles.get(id).copied().unwrap_or(0)
        }
    }

    impl BuildEnvironment for FakeEnvironment {
        fn unsupported(&self, module: &Module) -> Option<String> {
            let id = module.id.as_ref()?.as_str();
            self.refuse.get(id).cloned()
        }

        fn base_inputs(
            &self,
            module: &Module,
            _dependency_outputs: &[PathBuf],
        ) -> Result<CacheKeyInputs, String> {
            let id = module.id.as_ref().expect("id").as_str().to_string();
            let (body, signature) = self.sources.get(&id).cloned().unwrap_or_default();
            // The source digest covers BOTH: any edit changes this module's own key.
            Ok(CacheKeyInputs {
                compiler: "fake-compiler-1".into(),
                sources: vec![FileDigest::of_bytes(
                    format!("/src/{id}.kt"),
                    format!("{body}|{signature}").as_bytes(),
                )],
                target: "jvm-17".into(),
                ..CacheKeyInputs::default()
            })
        }

        fn compile(
            &mut self,
            module: &Module,
            dependency_outputs: &[PathBuf],
        ) -> Result<CompiledModule, String> {
            let id = module.id.as_ref().expect("id").as_str().to_string();
            self.dependency_outputs
                .insert(id.clone(), dependency_outputs.to_vec());
            *self.compiles.entry(id.clone()).or_insert(0) += 1;
            let (body, signature) = self.sources.get(&id).cloned().unwrap_or_default();
            // Body affects emitted bytes; only `signature` will reach the ABI (see `abi_of_fake`).
            let names = self
                .artifact_names
                .clone()
                .unwrap_or_else(|| vec![format!("{id}/Main.marker")]);
            let artifacts = names
                .into_iter()
                .map(|name| (name, format!("SIG:{signature}\nBODY:{body}\n").into_bytes()))
                .collect::<Vec<_>>();
            let published_abi = match &self.whole_output_reason {
                Some(reason) => PublishedAbi::WholeOutput {
                    reason: reason.clone(),
                },
                None => PublishedAbi::Exact(abi_of_fake(&artifacts)),
            };
            Ok(CompiledModule {
                artifacts,
                published_abi,
            })
        }
    }

    /// The fake environment owns this semantic identity: it reads the `SIG:` line and ignores
    /// `BODY:`, letting the production driver exercise exact-ABI avoidance without pretending its
    /// marker artifacts are JVM class files.
    fn abi_of_fake(artifacts: &[(String, Vec<u8>)]) -> AbiFingerprint {
        let mut rendered = String::new();
        for (path, bytes) in artifacts {
            let text = String::from_utf8_lossy(bytes);
            for line in text.lines().filter(|l| l.starts_with("SIG:")) {
                rendered.push_str(path);
                rendered.push(':');
                rendered.push_str(line);
                rendered.push('\n');
            }
        }
        AbiFingerprint::from_digest(crate::digest::digest_bytes(rendered.as_bytes()))
    }

    fn module(id: &str, deps: &[&str], output_root: &Path) -> Module {
        let mut m = Module::new(ModuleId::new(id), format!("/repo/{id}"));
        m.depends_on = deps.iter().map(|d| ModuleId::new(*d)).collect();
        m.outputs = vec![ModuleOutput::ClassDirectory(output_root.join(id))];
        m
    }

    /// core <- lib <- app
    fn chain(output_root: &Path) -> ModuleGraph {
        let mut graph = ModuleGraph::new();
        graph
            .insert(module("core", &[], output_root))
            .expect("core");
        graph
            .insert(module("lib", &["core"], output_root))
            .expect("lib");
        graph
            .insert(module("app", &["lib"], output_root))
            .expect("app");
        graph
    }

    fn driver(temp: &TempDir) -> Driver<FakeEnvironment> {
        let mut environment = FakeEnvironment::new(&temp.path().join("out"));
        environment.set("core", "body-v1", "sig-v1");
        environment.set("lib", "body-v1", "sig-v1");
        environment.set("app", "body-v1", "sig-v1");
        Driver::new(
            environment,
            ArtifactStore::open(temp.path().join("cache")).expect("store"),
        )
    }

    #[test]
    fn a_first_build_compiles_everything_in_dependency_order() {
        let temp = TempDir::new("first");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);

        let report = d.build(&graph).expect("plannable");
        assert!(report.is_success(), "{}", report.render());
        assert_eq!(report.compiled().len(), 3);
        assert!(report.cache_hits().is_empty());
        assert_eq!(
            report
                .outcomes
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "lib", "app"],
            "dependencies build before dependents"
        );
    }

    #[test]
    fn every_declared_dependency_output_reaches_the_compiler_in_order() {
        let temp = TempDir::new("all-dependency-outputs");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        let mut core = module("core", &[], &out);
        core.outputs = vec![
            ModuleOutput::ClassDirectory(out.join("core-classes")),
            ModuleOutput::ClassDirectory(out.join("core-generated")),
        ];
        let mut util = module("util", &[], &out);
        util.outputs = vec![
            ModuleOutput::ClassDirectory(out.join("util-classes")),
            ModuleOutput::ClassDirectory(out.join("util-generated")),
        ];
        graph.insert(core).expect("core");
        graph.insert(util).expect("util");
        graph
            .insert(module("app", &["util", "core"], &out))
            .expect("app");

        let mut environment = FakeEnvironment::new(&out);
        environment.set("core", "body", "signature");
        environment.set("util", "body", "signature");
        environment.set("app", "body", "signature");
        let store = ArtifactStore::open(temp.path().join("cache")).expect("store");
        let mut driver = Driver::new(environment, store);

        let report = driver.build(&graph).expect("plannable");
        assert!(report.is_success(), "{}", report.render());
        assert_eq!(
            driver.environment().dependency_outputs.get("app"),
            Some(&vec![
                out.join("util-classes"),
                out.join("util-generated"),
                out.join("core-classes"),
                out.join("core-generated"),
            ]),
            "dependency order and each dependency's output declaration order are preserved"
        );
    }

    #[test]
    fn an_unchanged_second_build_is_all_cache_hits() {
        let temp = TempDir::new("nochange");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);

        d.build(&graph).expect("plannable");
        let second = d.build(&graph).expect("plannable");

        assert_eq!(second.cache_hits().len(), 3, "{}", second.render());
        assert!(second.compiled().is_empty());
        for id in ["core", "lib", "app"] {
            assert_eq!(
                d.environment.compile_count(id),
                1,
                "{id} must not be compiled a second time"
            );
        }
    }

    /// The whole point of ABI keying: a body-only edit rebuilds ONE module, not its dependents.
    #[test]
    fn a_body_only_edit_rebuilds_only_that_module() {
        let temp = TempDir::new("bodyedit");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);
        d.build(&graph).expect("plannable");

        d.environment.set("core", "body-v2", "sig-v1"); // body changes, signature does not
        let report = d.build(&graph).expect("plannable");

        assert_eq!(
            report
                .compiled()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["core"],
            "only the edited module recompiles\n{}",
            report.render()
        );
        assert_eq!(
            report
                .cache_hits()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["lib", "app"],
            "dependents hit the cache because core's ABI did not move\n{}",
            report.render()
        );
        assert_eq!(d.environment.compile_count("lib"), 1);
        assert_eq!(d.environment.compile_count("app"), 1);
    }

    /// The other half: a signature edit must invalidate its DIRECT dependents.
    ///
    /// It propagates further only as far as ABIs keep changing, which is the point rather than a
    /// limitation. Here `core`'s signature moves, so `lib` — which compiles against it — rebuilds;
    /// but `lib`'s own declaration surface is untouched, so `app` is genuinely unaffected and hits
    /// the cache. That is compilation avoidance doing its job: the rebuild stops at the first
    /// module whose ABI does not move.
    ///
    /// The fake's ABI is derived from the module's own `SIG:` marker alone, so it cannot model a
    /// `lib` that re-exports a `core` type or inlines a `core` constant. The real implementation
    /// can once its semantic ABI publisher carries any `core` type in `lib`'s signatures, so such a
    /// `lib` would see its own ABI move and `app` would rebuild. See
    /// `a_signature_edit_reaches_a_module_that_also_depends_directly` for the graph-level case.
    #[test]
    fn a_signature_edit_rebuilds_dependents_until_an_abi_stops_changing() {
        let temp = TempDir::new("sigedit");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);
        d.build(&graph).expect("plannable");

        d.environment.set("core", "body-v1", "sig-v2");
        let report = d.build(&graph).expect("plannable");

        assert_eq!(
            report
                .compiled()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "lib"],
            "core's ABI moved, so its direct dependent rebuilds\n{}",
            report.render()
        );
        assert_eq!(
            report
                .cache_hits()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["app"],
            "lib's own ABI did not move, so app is genuinely unaffected\n{}",
            report.render()
        );
        assert_eq!(d.environment.compile_count("lib"), 2);
        assert_eq!(d.environment.compile_count("app"), 1);
    }

    /// When a module depends on the changed one DIRECTLY — the shape Gradle calls an `api`
    /// dependency, where the graph records the edge — the rebuild reaches it.
    #[test]
    fn a_signature_edit_reaches_a_module_that_also_depends_directly() {
        let temp = TempDir::new("sigedit-direct");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        graph.insert(module("core", &[], &out)).expect("core");
        graph.insert(module("lib", &["core"], &out)).expect("lib");
        // app sees core directly, not only through lib.
        graph
            .insert(module("app", &["lib", "core"], &out))
            .expect("app");

        let mut environment = FakeEnvironment::new(&out);
        for id in ["core", "lib", "app"] {
            environment.set(id, "body-v1", "sig-v1");
        }
        let mut d = Driver::new(
            environment,
            ArtifactStore::open(temp.path().join("cache")).expect("store"),
        );
        d.build(&graph).expect("plannable");

        d.environment.set("core", "body-v1", "sig-v2");
        let report = d.build(&graph).expect("plannable");

        assert_eq!(
            report
                .compiled()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "lib", "app"],
            "app carries core's ABI in its own key, so it rebuilds too\n{}",
            report.render()
        );
    }

    #[test]
    fn an_edit_to_a_leaf_does_not_disturb_its_dependencies() {
        let temp = TempDir::new("leafedit");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);
        d.build(&graph).expect("plannable");

        d.environment.set("app", "body-v2", "sig-v2");
        let report = d.build(&graph).expect("plannable");

        assert_eq!(
            report
                .compiled()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["app"]
        );
        assert_eq!(report.cache_hits().len(), 2);
    }

    /// A cache hit must leave the same files on disk a compile would have.
    #[test]
    fn a_cache_hit_still_materializes_output() {
        let temp = TempDir::new("materialize");
        let out = temp.path().join("out");
        let graph = chain(&out);
        let mut d = driver(&temp);

        d.build(&graph).expect("plannable");
        let produced = out.join("lib").join("lib").join("Main.marker");
        let after_compile = std::fs::read(&produced).expect("compile wrote output");

        std::fs::remove_dir_all(out.join("lib")).expect("wipe output");
        let report = d.build(&graph).expect("plannable");
        assert_eq!(report.cache_hits().len(), 3, "{}", report.render());

        let after_cache_hit = std::fs::read(&produced)
            .expect("a cache hit must restore output a dependent reads from disk");
        assert_eq!(after_compile, after_cache_hit);
    }

    #[test]
    fn a_module_with_java_sources_is_refused_and_blocks_its_dependents() {
        let temp = TempDir::new("java");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        let mut core = module("core", &[], &out);
        core.java_sources
            .push(PathBuf::from("/repo/core/Legacy.java"));
        graph.insert(core).expect("core");
        graph.insert(module("lib", &["core"], &out)).expect("lib");

        let store = ArtifactStore::open(temp.path().join("cache")).expect("store");
        let mut environment = FakeEnvironment::new(&out);
        environment.set("core", "b", "s");
        environment.set("lib", "b", "s");
        let mut real = Driver::new(environment, store);

        let report = real.build(&graph).expect("plannable");
        assert!(matches!(
            report.get(&ModuleId::new("core")),
            Some(Outcome::Refused { .. })
        ));
        assert!(
            matches!(
                report.get(&ModuleId::new("lib")),
                Some(Outcome::Blocked { .. })
            ),
            "a dependent of a refused module must not be built against missing output"
        );
        assert!(!report.is_success());
        assert_eq!(
            real.environment().compile_count("lib"),
            0,
            "a blocked module must never reach the compiler"
        );
    }

    #[test]
    fn a_cyclic_graph_is_refused_before_anything_is_built() {
        let temp = TempDir::new("cycle");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        graph.insert(module("a", &["b"], &out)).expect("a");
        graph.insert(module("b", &["a"], &out)).expect("b");

        let store = ArtifactStore::open(temp.path().join("cache")).expect("store");
        let mut real = Driver::new(FakeEnvironment::new(&out), store);
        assert!(matches!(real.build(&graph), Err(GraphError::Cycle { .. })));
    }

    /// A damaged cache must cost a recompile, never a failed build.
    #[test]
    fn a_corrupt_cache_entry_degrades_to_a_recompile() {
        let temp = TempDir::new("corrupt");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);
        let first = d.build(&graph).expect("plannable");

        let Some(Outcome::Compiled { key, .. }) = first.get(&ModuleId::new("core")) else {
            panic!("core should have compiled: {}", first.render());
        };
        std::fs::remove_file(d.store.root().join(key.to_string()).join("MANIFEST"))
            .expect("damage the entry");

        let second = d.build(&graph).expect("plannable");
        assert!(second.is_success(), "{}", second.render());
        assert_eq!(
            second
                .compiled()
                .iter()
                .map(|i| i.as_str())
                .collect::<Vec<_>>(),
            vec!["core"],
            "the damaged module recompiles; the rest still hit\n{}",
            second.render()
        );
        assert_eq!(d.environment.compile_count("core"), 2);
    }

    /// When the environment has no complete semantic ABI it publishes whole output, so a body-only
    /// edit reaches the dependent instead of serving a stale hit.
    #[test]
    fn a_body_edit_in_an_abi_incomplete_module_still_rebuilds_dependents() {
        let temp = TempDir::new("incomplete");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        graph.insert(module("core", &[], &out)).expect("core");
        graph.insert(module("app", &["core"], &out)).expect("app");

        let mut environment = FakeEnvironment::new(&out);
        environment.set("core", "body-v1", "sig-v1");
        environment.set("app", "body-v1", "sig-v1");
        environment.whole_output_reason = Some("semantic ABI unavailable".into());
        let store = ArtifactStore::open(temp.path().join("cache")).expect("store");
        let mut driver = Driver::new(environment, store);

        driver.build(&graph).expect("plannable");
        driver.environment.set("core", "body-v2", "sig-v1"); // body only — signature untouched
        let report = driver.build(&graph).expect("plannable");

        assert_eq!(
            report
                .compiled()
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "app"],
            "an inline body edit is invisible to the signature fingerprint, so an incomplete \
             module must publish a whole-output digest and rebuild its dependents\n{}",
            report.render()
        );
    }

    /// Finding: materialization overlaid rather than replaced, so a class that stopped being
    /// produced stayed on disk and remained visible to dependents.
    #[test]
    fn materialize_removes_artifacts_that_are_no_longer_produced() {
        let temp = TempDir::new("stale");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        graph.insert(module("core", &[], &out)).expect("core");

        let mut environment = FakeEnvironment::new(&out);
        environment.set("core", "body-v1", "sig-v1");
        environment.artifact_names =
            Some(vec!["pkg/Kept.marker".into(), "pkg/Removed.marker".into()]);
        let store = ArtifactStore::open(temp.path().join("cache")).expect("store");
        let mut driver = Driver::new(environment, store);
        driver.build(&graph).expect("plannable");

        let kept = out.join("core").join("pkg").join("Kept.marker");
        let removed = out.join("core").join("pkg").join("Removed.marker");
        assert!(
            kept.is_file() && removed.is_file(),
            "first build writes both"
        );

        // The module stops producing one class, as if its source were deleted.
        driver.environment.artifact_names = Some(vec!["pkg/Kept.marker".into()]);
        driver.environment.set("core", "body-v2", "sig-v1");
        driver.build(&graph).expect("plannable");

        assert!(kept.is_file(), "the surviving class is still there");
        assert!(
            !removed.exists(),
            "a class the module no longer produces must not be left on disk for dependents to \
             compile against"
        );
    }

    /// Finding: the model can express more than an environment implements; ignoring a field it
    /// promises yields a quietly wrong module cached under a valid-looking key.
    #[test]
    fn a_module_the_environment_cannot_honor_is_refused_and_blocks_dependents() {
        let temp = TempDir::new("unsupported");
        let out = temp.path().join("out");
        let mut graph = ModuleGraph::new();
        graph.insert(module("core", &[], &out)).expect("core");
        graph.insert(module("app", &["core"], &out)).expect("app");

        let mut environment = FakeEnvironment::new(&out);
        environment.set("core", "b", "s");
        environment.set("app", "b", "s");
        environment.refuse.insert(
            "core".into(),
            "declares resources this environment does not copy".into(),
        );
        let store = ArtifactStore::open(temp.path().join("cache")).expect("store");
        let mut driver = Driver::new(environment, store);

        let report = driver.build(&graph).expect("plannable");
        assert!(matches!(
            report.get(&ModuleId::new("core")),
            Some(Outcome::Refused { .. })
        ));
        assert!(matches!(
            report.get(&ModuleId::new("app")),
            Some(Outcome::Blocked { .. })
        ));
        assert_eq!(
            driver.environment.compile_count("core"),
            0,
            "a refused module must never reach the compiler, and never be cached"
        );
        assert!(
            driver.store.is_empty().expect("store readable"),
            "nothing may be cached for a refused build"
        );
    }

    #[test]
    fn the_report_renders_one_line_per_module() {
        let temp = TempDir::new("render");
        let graph = chain(&temp.path().join("out"));
        let mut d = driver(&temp);
        let rendered = d.build(&graph).expect("plannable").render();
        assert_eq!(rendered.lines().count(), 3);
        assert!(rendered.contains("compiled  core"));
    }
}
