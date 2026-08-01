//! Source roots, classpath, module graph, and JDK for one worktree.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Stable identity of one compilation unit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(String);

impl ModuleId {
    pub fn new(module: &str, source_set: &str) -> Self {
        Self(format!("{module}:{source_set}"))
    }

    /// An id whose form is decided by the producer (e.g. a BSP build-target URI), used verbatim.
    pub fn raw(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a source root holds production or test sources, and whether a build step produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceRootKind {
    Source,
    Test,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRoot {
    pub path: PathBuf,
    pub kind: SourceRootKind,
    pub generated: bool,
    pub package_prefix: String,
}

impl SourceRoot {
    pub fn source(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: SourceRootKind::Source,
            generated: false,
            package_prefix: String::new(),
        }
    }

    pub fn test(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: SourceRootKind::Test,
            generated: false,
            package_prefix: String::new(),
        }
    }

    pub fn generated(mut self) -> Self {
        self.generated = true;
        self
    }

    pub fn with_package_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.package_prefix = prefix.into();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleOutput {
    Classes(PathBuf),
    Location(PathBuf),
}

impl ModuleOutput {
    pub fn classes(path: impl Into<PathBuf>) -> Self {
        Self::Classes(path.into())
    }

    pub fn location(path: impl Into<PathBuf>) -> Self {
        Self::Location(path.into())
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Classes(path) | Self::Location(path) => path,
        }
    }

    fn classpath_entry(&self) -> Option<&Path> {
        match self {
            Self::Classes(path) => Some(path),
            Self::Location(_) => None,
        }
    }
}

/// One compilation unit of the project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub id: Option<ModuleId>,
    pub display_name: String,
    pub base_directory: PathBuf,
    pub source_roots: Vec<SourceRoot>,
    /// Resolved compile classpath: jars and class directories, in build-tool order.
    pub classpath: Vec<PathBuf>,
    pub outputs: Vec<ModuleOutput>,
    pub depends_on: Vec<ModuleId>,
    /// BSP `associates`: outputs whose `internal` declarations this module may see.
    pub friend_paths: Vec<PathBuf>,
    pub jvm_target: Option<String>,
    pub kotlinc_args: Vec<String>,
}

impl Module {
    pub fn new(id: ModuleId, base_directory: impl Into<PathBuf>) -> Self {
        Self {
            display_name: id.as_str().to_string(),
            id: Some(id),
            base_directory: base_directory.into(),
            ..Self::default()
        }
    }
}

/// Which producer built the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// A classpath was passed on the command line; no build tool is consulted.
    Explicit,
    /// A Build Server Protocol server advertised via `.bsp/*.json`.
    Bsp,
    Gradle,
    Maven,
    /// A JetBrains project model described under `.idea/`; no build tool is consulted.
    Jps,
    /// No build system was found: builtins, the JDK, and any local jar directories.
    None,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Explicit => "explicit",
            ProviderKind::Bsp => "bsp",
            ProviderKind::Gradle => "gradle",
            ProviderKind::Maven => "maven",
            ProviderKind::Jps => "jps",
            ProviderKind::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectModel {
    pub root: PathBuf,
    pub kind: ProviderKind,
    pub jdk_home: Option<PathBuf>,
    pub modules: Vec<Module>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceModuleRelations {
    pub dependencies: Vec<usize>,
    pub friends: Vec<usize>,
}

impl SourceModuleRelations {
    pub fn visible(&self) -> Vec<usize> {
        let mut visible = self.dependencies.clone();
        visible.extend(self.friends.iter().copied());
        visible.sort_unstable();
        visible.dedup();
        visible
    }
}

#[derive(Clone, Debug)]
pub struct SourceModuleGraph {
    model: Arc<ProjectModel>,
    relations: Arc<[SourceModuleRelations]>,
    source_roots: Arc<SourceRootIndex>,
    cache_key: SourceModuleGraphKey,
}

#[derive(Debug, Default)]
struct SourceRootIndex {
    nodes: Vec<SourceRootIndexNode>,
}

#[derive(Debug, Default)]
struct SourceRootIndexNode {
    children: HashMap<OsString, usize>,
    owner: Option<(usize, usize)>,
}

impl SourceRootIndex {
    /// Build the same longest-component-prefix relation exposed by
    /// [`ProjectModel::module_source_root_for_source`] once for the immutable graph snapshot.
    ///
    /// Owners are stored as model indices rather than cloned roots so the index cannot drift from
    /// package prefixes or other root metadata held by the snapshot. Inserting in model order is
    /// also significant: replacing an existing owner makes an identical later root win, matching
    /// the linear reference implementation's `(depth, module_index, root_index)` tie-break.
    fn new(modules: &[Module]) -> Self {
        let mut index = Self {
            nodes: vec![SourceRootIndexNode::default()],
        };
        for (module_index, module) in modules.iter().enumerate() {
            for (root_index, root) in module.source_roots.iter().enumerate() {
                let mut node_index = 0;
                for component in root.path.components() {
                    let component = component.as_os_str().to_os_string();
                    let child = index.nodes[node_index].children.get(&component).copied();
                    node_index = match child {
                        Some(child) => child,
                        None => {
                            let child = index.nodes.len();
                            index.nodes.push(SourceRootIndexNode::default());
                            index.nodes[node_index].children.insert(component, child);
                            child
                        }
                    };
                }
                index.nodes[node_index].owner = Some((module_index, root_index));
            }
        }
        index
    }

    fn module_source_root_for_source(&self, path: &Path) -> Option<(usize, usize)> {
        let mut node_index = 0;
        let mut owner = self.nodes[node_index].owner;
        for component in path.components() {
            let Some(child) = self.nodes[node_index].children.get(component.as_os_str()) else {
                break;
            };
            node_index = *child;
            owner = self.nodes[node_index].owner.or(owner);
        }
        owner
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceModuleGraphKey(Arc<SourceModuleGraphKeyData>);

#[derive(Debug, PartialEq, Eq)]
struct SourceModuleGraphKeyData {
    root: PathBuf,
    kind: ProviderKind,
    modules: Vec<SourceModuleGraphModuleKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceModuleGraphModuleKey {
    id: Option<ModuleId>,
    base_directory: PathBuf,
    source_roots: Vec<SourceRoot>,
    outputs: Vec<ModuleOutput>,
    depends_on: Vec<ModuleId>,
    friend_paths: Vec<PathBuf>,
    relations: SourceModuleRelations,
}

impl SourceModuleGraph {
    pub fn new(model: ProjectModel) -> Self {
        Self::new_with(model, &|path| std::fs::canonicalize(path).ok())
    }

    fn new_with(model: ProjectModel, canonicalize: &dyn Fn(&Path) -> Option<PathBuf>) -> Self {
        let source_roots = SourceRootIndex::new(&model.modules);
        let mut module_ids = HashMap::<&ModuleId, Vec<usize>>::new();
        let mut outputs = HashMap::<&Path, Vec<usize>>::new();
        for (index, module) in model.modules.iter().enumerate() {
            if let Some(id) = module.id.as_ref() {
                module_ids.entry(id).or_default().push(index);
            }
            for output in &module.outputs {
                outputs.entry(output.path()).or_default().push(index);
            }
        }

        let has_friend_paths = model
            .modules
            .iter()
            .any(|module| !module.friend_paths.is_empty());
        let mut canonical_paths = CanonicalPathCache::new(canonicalize);
        let mut canonical_outputs = HashMap::<PathBuf, Vec<usize>>::new();
        if has_friend_paths {
            for (output, indices) in &outputs {
                if let Some(canonical) = canonical_paths.get(output) {
                    canonical_outputs
                        .entry(canonical.to_path_buf())
                        .or_default()
                        .extend(indices.iter().copied());
                }
            }
        }

        let relations: Vec<SourceModuleRelations> = model
            .modules
            .iter()
            .enumerate()
            .map(|(module_index, module)| {
                let mut dependencies = module
                    .depends_on
                    .iter()
                    .filter_map(|id| module_ids.get(id))
                    .flatten()
                    .copied()
                    .filter(|index| *index != module_index)
                    .collect::<Vec<_>>();
                dependencies.sort_unstable();
                dependencies.dedup();

                let mut friends = Vec::new();
                for friend in &module.friend_paths {
                    if let Some(indices) = outputs.get(friend.as_path()) {
                        friends.extend(indices.iter().copied());
                    }
                    if let Some(indices) = canonical_paths
                        .get(friend)
                        .and_then(|friend| canonical_outputs.get(friend))
                    {
                        friends.extend(indices.iter().copied());
                    }
                }
                friends.retain(|index| *index != module_index);
                friends.sort_unstable();
                friends.dedup();
                SourceModuleRelations {
                    dependencies,
                    friends,
                }
            })
            .collect();
        let cache_key = SourceModuleGraphKey(Arc::new(SourceModuleGraphKeyData {
            root: model.root.clone(),
            kind: model.kind,
            modules: model
                .modules
                .iter()
                .zip(&relations)
                .map(|(module, relations)| SourceModuleGraphModuleKey {
                    id: module.id.clone(),
                    base_directory: module.base_directory.clone(),
                    source_roots: module.source_roots.clone(),
                    outputs: module.outputs.clone(),
                    depends_on: module.depends_on.clone(),
                    friend_paths: module.friend_paths.clone(),
                    relations: relations.clone(),
                })
                .collect(),
        }));
        Self {
            model: Arc::new(model),
            relations: relations.into(),
            source_roots: Arc::new(source_roots),
            cache_key,
        }
    }

    pub fn model(&self) -> &ProjectModel {
        self.model.as_ref()
    }

    pub fn get(&self, module_index: usize) -> Option<&SourceModuleRelations> {
        self.relations.get(module_index)
    }

    pub fn module_index_for_source(&self, path: &Path) -> Option<usize> {
        self.source_roots
            .module_source_root_for_source(path)
            .map(|(module_index, _)| module_index)
    }

    pub fn module_source_root_for_source(&self, path: &Path) -> Option<(usize, &SourceRoot)> {
        let (module_index, root_index) = self.source_roots.module_source_root_for_source(path)?;
        let root = self
            .model
            .modules
            .get(module_index)?
            .source_roots
            .get(root_index)?;
        Some((module_index, root))
    }

    pub fn iter(&self) -> impl Iterator<Item = &SourceModuleRelations> {
        self.relations.iter()
    }

    pub(super) fn cache_key(&self) -> &SourceModuleGraphKey {
        &self.cache_key
    }
}

pub(super) struct CanonicalPathCache<'canonicalize> {
    canonicalize: &'canonicalize dyn Fn(&Path) -> Option<PathBuf>,
    paths: HashMap<PathBuf, Option<PathBuf>>,
}

impl<'canonicalize> CanonicalPathCache<'canonicalize> {
    pub(super) fn new(canonicalize: &'canonicalize dyn Fn(&Path) -> Option<PathBuf>) -> Self {
        Self {
            canonicalize,
            paths: HashMap::new(),
        }
    }

    pub(super) fn get(&mut self, path: &Path) -> Option<&Path> {
        if !self.paths.contains_key(path) {
            let canonical = (self.canonicalize)(path);
            self.paths.insert(path.to_path_buf(), canonical);
        }
        self.paths.get(path).and_then(Option::as_deref)
    }
}

impl ProjectModel {
    pub fn new(root: impl Into<PathBuf>, kind: ProviderKind) -> Self {
        Self {
            root: root.into(),
            kind,
            jdk_home: None,
            modules: Vec::new(),
        }
    }

    pub fn with_modules(mut self, modules: Vec<Module>) -> Self {
        self.modules = modules;
        self
    }

    pub fn module(&self, id: &ModuleId) -> Option<&Module> {
        self.modules
            .iter()
            .find(|module| module.id.as_ref() == Some(id))
    }

    pub fn module_index_for_source(&self, path: &Path) -> Option<usize> {
        self.module_source_root_for_source(path)
            .map(|(module_index, _)| module_index)
    }

    pub fn module_source_root_for_source(&self, path: &Path) -> Option<(usize, &SourceRoot)> {
        self.modules
            .iter()
            .enumerate()
            .flat_map(|(module_index, module)| {
                module
                    .source_roots
                    .iter()
                    .enumerate()
                    .filter_map(move |(root_index, root)| {
                        path.starts_with(&root.path).then_some((
                            root.path.components().count(),
                            module_index,
                            root_index,
                            root,
                        ))
                    })
            })
            .max_by_key(|(depth, module_index, root_index, _)| (*depth, *module_index, *root_index))
            .map(|(_, module_index, _, root)| (module_index, root))
    }

    pub fn module_for_source(&self, path: &Path) -> Option<&Module> {
        self.module_index_for_source(path)
            .and_then(|index| self.modules.get(index))
    }

    pub fn into_source_module_graph(self) -> SourceModuleGraph {
        SourceModuleGraph::new(self)
    }

    /// Classpath handed to the compiler for `module`, deduplicated in build-tool order.
    pub fn compile_classpath(&self, module: &Module) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = Vec::new();
        let push = |entry: &Path, entries: &mut Vec<PathBuf>| {
            if !entries.iter().any(|existing| existing == entry) {
                entries.push(entry.to_path_buf());
            }
        };
        for entry in &module.classpath {
            push(entry, &mut entries);
        }
        for entry in &module.friend_paths {
            push(entry, &mut entries);
        }
        for dependency in &module.depends_on {
            if let Some(dependency) = self.module(dependency) {
                for output in &dependency.outputs {
                    if let Some(entry) = output.classpath_entry() {
                        push(entry, &mut entries);
                    }
                }
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ProjectModel {
        let mut core = Module::new(ModuleId::new(":core", "main"), "/p/core");
        core.source_roots = vec![SourceRoot::source("/p/core/src/main/kotlin")];
        core.outputs = vec![
            ModuleOutput::classes("/p/core/build/classes/java/main"),
            ModuleOutput::classes("/p/core/build/classes/kotlin/main"),
        ];
        core.classpath = vec![PathBuf::from("/m2/kotlin-stdlib.jar")];

        let mut generated = Module::new(ModuleId::new(":generated", "main"), "/p/generated");
        generated.outputs = vec![ModuleOutput::location("/p/generated/build")];

        let mut app = Module::new(ModuleId::new(":app", "main"), "/p/app");
        app.source_roots = vec![SourceRoot::source("/p/app/src/main/kotlin")];
        app.outputs = vec![ModuleOutput::classes("/p/app/build/classes/kotlin/main")];
        app.classpath = vec![PathBuf::from("/m2/kotlin-stdlib.jar")];
        app.depends_on = vec![
            ModuleId::new(":core", "main"),
            ModuleId::new(":generated", "main"),
        ];

        let mut app_test = Module::new(ModuleId::new(":app", "test"), "/p/app");
        app_test.source_roots = vec![SourceRoot::test("/p/app/src/test/kotlin")];
        app_test.depends_on = vec![ModuleId::new(":app", "main")];
        app_test.friend_paths = vec![PathBuf::from("/p/app/build/classes/kotlin/main")];

        ProjectModel::new("/p", ProviderKind::Gradle)
            .with_modules(vec![core, generated, app, app_test])
    }

    #[test]
    fn compile_classpath_adds_class_outputs_but_not_opaque_locations() {
        let model = model();
        let app = model.module(&ModuleId::new(":app", "main")).unwrap();
        assert_eq!(
            model.compile_classpath(app),
            vec![
                PathBuf::from("/m2/kotlin-stdlib.jar"),
                PathBuf::from("/p/core/build/classes/java/main"),
                PathBuf::from("/p/core/build/classes/kotlin/main"),
            ]
        );

        let test = model.module(&ModuleId::new(":app", "test")).unwrap();
        // The friend path and the dependency output are the same directory: it appears once.
        assert_eq!(
            model.compile_classpath(test),
            vec![PathBuf::from("/p/app/build/classes/kotlin/main")]
        );
    }

    #[test]
    fn source_paths_select_the_module_with_the_most_specific_root() {
        let mut model = model();
        model.modules[2]
            .source_roots
            .push(SourceRoot::source("/p/shared").with_package_prefix("first"));
        model.modules[3].source_roots.extend([
            SourceRoot::source("/p/shared").with_package_prefix("second"),
            SourceRoot::source("/p/shared/generated"),
        ]);
        let graph = model.clone().into_source_module_graph();
        let test_source = Path::new("/p/app/src/test/kotlin/Example.kt");

        assert_eq!(
            model
                .module_for_source(test_source)
                .and_then(|module| module.id.as_ref())
                .map(ModuleId::as_str),
            Some(":app:test")
        );
        assert_eq!(model.module_index_for_source(test_source), Some(3));
        assert_eq!(graph.module_index_for_source(test_source), Some(3));
        assert_eq!(
            graph.module_index_for_source(Path::new("/p/app/src/main/kotlin/Example.kt")),
            Some(2)
        );
        assert_eq!(
            graph.module_index_for_source(Path::new("/p/unowned/Example.kt")),
            None
        );
        let (module_index, root) = model
            .module_source_root_for_source(Path::new("/p/shared/Example.java"))
            .unwrap();
        assert_eq!(module_index, 3);
        assert_eq!(root.package_prefix, "second");
        assert_eq!(
            graph.module_source_root_for_source(Path::new("/p/shared/Example.java")),
            Some((module_index, root))
        );
        let (_, root) = model
            .module_source_root_for_source(Path::new("/p/shared/generated/Example.java"))
            .unwrap();
        assert!(root.package_prefix.is_empty());
        assert_eq!(
            graph.module_source_root_for_source(Path::new("/p/shared/generated/Example.java")),
            Some((3, root))
        );
        assert!(model
            .module_for_source(Path::new("/p/unowned/Example.kt"))
            .is_none());
    }

    #[test]
    fn source_root_index_matches_linear_lookup_for_duplicate_and_nested_roots() {
        let mut model = model();
        let mut duplicate = Module::new(ModuleId::new(":duplicate", "main"), "/p/core");
        duplicate.source_roots = vec![SourceRoot::source("/p/core/src/main/kotlin")];
        model.modules.push(duplicate);
        let graph = model.clone().into_source_module_graph();

        for path in [
            "/p/core/src/main/kotlin/Core.kt",
            "/p/app/src/main/kotlin/App.kt",
            "/p/app/src/test/kotlin/AppTest.kt",
            "/p/unowned/Other.kt",
        ] {
            let path = Path::new(path);
            assert_eq!(
                graph.module_index_for_source(path),
                model.module_index_for_source(path)
            );
            assert_eq!(
                graph.module_source_root_for_source(path),
                model.module_source_root_for_source(path)
            );
        }
    }

    #[test]
    fn visible_source_modules_include_dependencies_and_exact_friend_outputs_once() {
        let mut model = model();
        model.modules[1].outputs = vec![ModuleOutput::classes("/p/generated/classes")];
        model.modules[2].friend_paths = vec![PathBuf::from("/p/generated/classes")];
        let graph = model.into_source_module_graph();

        assert_eq!(graph.get(2).unwrap().dependencies, [0, 1]);
        assert_eq!(graph.get(2).unwrap().friends, [1]);
        assert_eq!(graph.get(2).unwrap().visible(), [0, 1]);
        assert_eq!(graph.get(3).unwrap().dependencies, [2]);
        assert_eq!(graph.get(3).unwrap().friends, [2]);
        assert_eq!(graph.get(3).unwrap().visible(), [2]);
    }

    #[cfg(unix)]
    #[test]
    fn friend_source_modules_match_canonically_equivalent_output_paths() {
        use std::os::unix::fs::symlink;

        use crate::project::testing::TempTree;

        let tree = TempTree::new("model-friend-output-symlink");
        let output = tree.path("real/classes");
        let linked_output = tree.path("linked-classes");
        std::fs::create_dir_all(&output).unwrap();
        symlink(&output, &linked_output).unwrap();

        let mut producer = Module::new(ModuleId::new(":producer", "main"), tree.path("producer"));
        producer.outputs = vec![ModuleOutput::classes(output)];
        let mut consumer = Module::new(ModuleId::new(":consumer", "test"), tree.path("consumer"));
        consumer.friend_paths = vec![linked_output];
        let model = ProjectModel::new(tree.root(), ProviderKind::Gradle)
            .with_modules(vec![producer, consumer]);
        let graph = model.into_source_module_graph();

        assert_eq!(graph.get(1).unwrap().friends, [0]);
    }

    #[test]
    fn source_module_graph_memoizes_duplicate_paths() {
        use std::cell::Cell;

        let mut modules = Vec::new();
        for index in 0..128 {
            let mut producer = Module::new(
                ModuleId::new(&format!(":producer-{index}"), "main"),
                format!("/workspace/producer-{index}"),
            );
            producer.outputs = vec![ModuleOutput::classes(format!("/declared/output-{index}"))];
            modules.push(producer);
        }
        let mut consumer = Module::new(ModuleId::new(":consumer", "test"), "/workspace/consumer");
        consumer.friend_paths = (0..128)
            .flat_map(|index| {
                let path = PathBuf::from(format!("/alias/output-{index}"));
                [path.clone(), path]
            })
            .collect();
        modules.push(consumer);
        let model = ProjectModel::new("/workspace", ProviderKind::Gradle).with_modules(modules);
        let calls = Cell::new(0usize);
        let graph = SourceModuleGraph::new_with(model, &|path| {
            calls.set(calls.get() + 1);
            let name = path.file_name()?.to_str()?;
            Some(PathBuf::from("/canonical").join(name))
        });

        assert_eq!(
            graph.get(128).unwrap().friends,
            (0..128).collect::<Vec<_>>()
        );
        assert_eq!(calls.get(), 256);
    }

    #[test]
    fn friend_relations_include_exact_and_canonical_output_matches() {
        let mut exact = Module::new(ModuleId::new(":exact", "main"), "/workspace/exact");
        exact.outputs = vec![ModuleOutput::classes("/declared/shared")];
        let mut alias = Module::new(ModuleId::new(":alias", "main"), "/workspace/alias");
        alias.outputs = vec![ModuleOutput::classes("/alias/shared")];
        let mut consumer = Module::new(ModuleId::new(":consumer", "test"), "/workspace/consumer");
        consumer.friend_paths = vec![PathBuf::from("/declared/shared")];
        let model = ProjectModel::new("/workspace", ProviderKind::Gradle)
            .with_modules(vec![exact, alias, consumer]);

        let graph = SourceModuleGraph::new_with(model, &|path| {
            (path.file_name()?.to_str()? == "shared").then(|| PathBuf::from("/canonical/shared"))
        });

        assert_eq!(graph.get(2).unwrap().friends, [0, 1]);
    }
}
