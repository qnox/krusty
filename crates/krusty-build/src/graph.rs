//! The module DAG: what order to build in, and what a changed ABI invalidates.
//!
//! Two queries carry the whole build model. [`ModuleGraph::build_order`] says what may compile when
//! (and refuses a cyclic graph rather than deadlocking on it). [`ModuleGraph::transitive_dependents`]
//! says which modules a changed ABI forces to rebuild — the query that makes ABI-keyed avoidance
//! worth anything, since without it a change invalidates everything.
//!
//! Ordering is deterministic. Two runs over the same graph must produce the same order, for the
//! same reason emission must be byte-identical: a build whose plan varies run to run cannot be
//! compared against a clean rebuild, which is the driver's oracle.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::model::{Module, ModuleId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// A module declares a dependency on an id the graph does not contain.
    UnknownDependency {
        module: ModuleId,
        dependency: ModuleId,
    },
    /// The graph is cyclic. `cycle` lists the modules that remained unresolved, sorted, so the
    /// message is deterministic and diffable.
    Cycle { cycle: Vec<ModuleId> },
    /// Two modules claim the same id.
    DuplicateModule { module: ModuleId },
    /// A module was added without an id.
    MissingId { display_name: String },
    /// The module base must make every relative child path independent of process cwd.
    RelativeBaseDirectory { module: ModuleId, base: PathBuf },
    /// Two declared outputs have the same or ancestor/descendant ownership.
    OverlappingOutputs {
        first_module: ModuleId,
        first_output: PathBuf,
        second_module: ModuleId,
        second_output: PathBuf,
    },
    /// Materializing a filesystem root would replace an unbounded tree.
    FilesystemRootOutput { module: ModuleId, output: PathBuf },
    /// Materializing an output would replace a declared compiler/build input.
    OutputOverlapsInput {
        output_module: ModuleId,
        output: PathBuf,
        input_module: ModuleId,
        input: PathBuf,
    },
    /// A friend output owned by this graph must be built before the friend module.
    FriendOwnerNotDependency {
        module: ModuleId,
        friend_path: PathBuf,
        owner: ModuleId,
    },
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDependency { module, dependency } => {
                write!(f, "module {module} depends on unknown module {dependency}")
            }
            Self::Cycle { cycle } => {
                let names: Vec<&str> = cycle.iter().map(ModuleId::as_str).collect();
                write!(f, "module dependency cycle among: {}", names.join(", "))
            }
            Self::DuplicateModule { module } => write!(f, "duplicate module id {module}"),
            Self::MissingId { display_name } => {
                write!(f, "module {display_name} has no id")
            }
            Self::RelativeBaseDirectory { module, base } => write!(
                f,
                "module {module} has relative base directory {}",
                base.display()
            ),
            Self::OverlappingOutputs {
                first_module,
                first_output,
                second_module,
                second_output,
            } => write!(
                f,
                "module {first_module} output {} overlaps module {second_module} output {}",
                first_output.display(),
                second_output.display()
            ),
            Self::FilesystemRootOutput { module, output } => write!(
                f,
                "module {module} output {} is a filesystem root",
                output.display()
            ),
            Self::OutputOverlapsInput {
                output_module,
                output,
                input_module,
                input,
            } => write!(
                f,
                "module {output_module} output {} overlaps module {input_module} input {}",
                output.display(),
                input.display()
            ),
            Self::FriendOwnerNotDependency {
                module,
                friend_path,
                owner,
            } => write!(
                f,
                "module {module} uses friend output {} from {owner} without depending on it",
                friend_path.display()
            ),
        }
    }
}

/// Modules keyed by id. `BTreeMap` rather than `HashMap` so iteration — and therefore every order
/// derived from it — is deterministic without a sort at each use.
#[derive(Clone, Debug, Default)]
pub struct ModuleGraph {
    modules: BTreeMap<ModuleId, Module>,
}

impl ModuleGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one module. Fails on a missing or duplicate id rather than silently overwriting — a
    /// build that quietly dropped a module would produce a short output with no diagnostic.
    pub fn insert(&mut self, mut module: Module) -> Result<(), GraphError> {
        let Some(id) = module.id.clone() else {
            return Err(GraphError::MissingId {
                display_name: module.display_name.clone(),
            });
        };
        if self.modules.contains_key(&id) {
            return Err(GraphError::DuplicateModule { module: id });
        }
        if let Err(base) = module.normalize_paths() {
            return Err(GraphError::RelativeBaseDirectory { module: id, base });
        }

        // Project-model providers may report the same edge through more than one imported model.
        // A graph edge is a relation, not a count: retaining a duplicate here while the reverse
        // index below uses a set makes Kahn's indegree impossible to discharge and reports a false
        // cycle. Canonicalize once at the ownership boundary, preserving the provider's first-seen
        // order for deterministic classpath construction.
        let mut seen_dependencies = BTreeSet::new();
        module
            .depends_on
            .retain(|dependency| seen_dependencies.insert(dependency.clone()));
        self.modules.insert(id, module);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn get(&self, id: &ModuleId) -> Option<&Module> {
        self.modules.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &ModuleId> {
        self.modules.keys()
    }

    /// Every declared dependency resolves to a module in this graph.
    pub fn validate(&self) -> Result<(), GraphError> {
        for (id, module) in &self.modules {
            for dependency in &module.depends_on {
                if !self.modules.contains_key(dependency) {
                    return Err(GraphError::UnknownDependency {
                        module: id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        let mut outputs: Vec<(&ModuleId, &Path)> = Vec::new();
        for (module, declaration) in &self.modules {
            for output in &declaration.outputs {
                let path = output.path();
                if path.parent().is_none() {
                    return Err(GraphError::FilesystemRootOutput {
                        module: module.clone(),
                        output: path.to_path_buf(),
                    });
                }
                for (other_module, other_path) in &outputs {
                    if paths_overlap(path, other_path) {
                        return Err(GraphError::OverlappingOutputs {
                            first_module: (*other_module).clone(),
                            first_output: other_path.to_path_buf(),
                            second_module: module.clone(),
                            second_output: path.to_path_buf(),
                        });
                    }
                }
                outputs.push((module, path));
            }
        }
        for (output_module, output) in &outputs {
            for (input_module, declaration) in &self.modules {
                let protected_inputs = declaration
                    .source_roots
                    .iter()
                    .map(|root| root.path.as_path())
                    .chain(declaration.resources.iter().map(PathBuf::as_path))
                    .chain(declaration.java_sources.iter().map(PathBuf::as_path))
                    .chain(declaration.processor_path.iter().map(PathBuf::as_path))
                    .chain(declaration.jdk_home.iter().map(PathBuf::as_path));
                for input in protected_inputs {
                    if paths_overlap(output, input) {
                        return Err(GraphError::OutputOverlapsInput {
                            output_module: (*output_module).clone(),
                            output: output.to_path_buf(),
                            input_module: input_module.clone(),
                            input: input.to_path_buf(),
                        });
                    }
                }
                // A declared output may intentionally be another module's classpath/friend root.
                // Every other explicit path is immutable input and must not be replaced.
                for input in declaration
                    .classpath
                    .iter()
                    .chain(&declaration.friend_paths)
                {
                    if outputs
                        .iter()
                        .any(|(_, declared_output)| *declared_output == input)
                    {
                        continue;
                    }
                    if paths_overlap(output, input) {
                        return Err(GraphError::OutputOverlapsInput {
                            output_module: (*output_module).clone(),
                            output: output.to_path_buf(),
                            input_module: input_module.clone(),
                            input: input.clone(),
                        });
                    }
                }
            }
        }
        for (module, declaration) in &self.modules {
            for friend_path in &declaration.friend_paths {
                let Some((owner, _)) = outputs.iter().find(|(_, output)| *output == friend_path)
                else {
                    continue;
                };
                if *owner != module && !declaration.depends_on.contains(owner) {
                    return Err(GraphError::FriendOwnerNotDependency {
                        module: module.clone(),
                        friend_path: friend_path.clone(),
                        owner: (*owner).clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Modules in dependency order: every module appears after all of its dependencies.
    ///
    /// Kahn's algorithm over a `BTreeMap`, so ties break by id and the order is identical across
    /// runs. A cyclic graph is an error, never a partial order — a driver that built "as much as it
    /// could" of a cycle would emit artifacts compiled against a half-built dependency.
    pub fn build_order(&self) -> Result<Vec<ModuleId>, GraphError> {
        self.validate()?;

        let mut remaining: BTreeMap<&ModuleId, usize> = self
            .modules
            .iter()
            .map(|(id, module)| (id, module.depends_on.len()))
            .collect();

        // id -> modules that depend on it, so resolving one can decrement its dependents.
        let mut dependents: BTreeMap<&ModuleId, BTreeSet<&ModuleId>> = BTreeMap::new();
        for (id, module) in &self.modules {
            for dependency in &module.depends_on {
                dependents.entry(dependency).or_default().insert(id);
            }
        }

        let mut ready: VecDeque<&ModuleId> = remaining
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut order = Vec::with_capacity(self.modules.len());

        while let Some(id) = ready.pop_front() {
            remaining.remove(id);
            order.push(id.clone());
            let Some(waiting) = dependents.get(id) else {
                continue;
            };
            for dependent in waiting {
                if let Some(count) = remaining.get_mut(*dependent) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push_back(*dependent);
                    }
                }
            }
        }

        if !remaining.is_empty() {
            let mut cycle: Vec<ModuleId> = remaining.keys().map(|id| (*id).clone()).collect();
            cycle.sort();
            return Err(GraphError::Cycle { cycle });
        }
        Ok(order)
    }

    /// Direct dependencies of `id`, in declaration order.
    pub fn direct_dependencies(&self, id: &ModuleId) -> &[ModuleId] {
        self.modules
            .get(id)
            .map(|module| module.depends_on.as_slice())
            .unwrap_or(&[])
    }

    /// Every module that must rebuild when `id`'s ABI changes — its dependents, transitively,
    /// excluding `id` itself. Sorted, so a rebuild plan is stable.
    ///
    /// This tolerates a cyclic graph deliberately: it is used to explain and to invalidate, and
    /// refusing to answer because some unrelated corner of the graph has a cycle would be worse
    /// than answering. [`Self::build_order`] is where a cycle is fatal.
    pub fn transitive_dependents(&self, id: &ModuleId) -> Vec<ModuleId> {
        let mut direct: BTreeMap<&ModuleId, BTreeSet<&ModuleId>> = BTreeMap::new();
        for (owner, module) in &self.modules {
            for dependency in &module.depends_on {
                direct.entry(dependency).or_default().insert(owner);
            }
        }

        let mut seen: BTreeSet<&ModuleId> = BTreeSet::new();
        let mut queue: VecDeque<&ModuleId> = VecDeque::new();
        queue.push_back(id);
        while let Some(current) = queue.pop_front() {
            let Some(waiting) = direct.get(current) else {
                continue;
            };
            for dependent in waiting {
                if seen.insert(*dependent) {
                    queue.push_back(*dependent);
                }
            }
        }
        seen.remove(id);
        seen.into_iter().cloned().collect()
    }
}

fn paths_overlap(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(id: &str, depends_on: &[&str]) -> Module {
        let mut m = Module::new(ModuleId::new(id), format!("/repo/{id}"));
        m.depends_on = depends_on.iter().map(|d| ModuleId::new(*d)).collect();
        m
    }

    fn graph(specs: &[(&str, &[&str])]) -> ModuleGraph {
        let mut g = ModuleGraph::new();
        for (id, deps) in specs {
            g.insert(module(id, deps)).expect("insert");
        }
        g
    }

    #[test]
    fn build_order_places_dependencies_first() {
        let g = graph(&[("app", &["lib"]), ("lib", &["core"]), ("core", &[])]);
        let order = g.build_order().expect("acyclic");
        let position = |id: &str| order.iter().position(|m| m.as_str() == id).unwrap();
        assert!(position("core") < position("lib"));
        assert!(position("core") < position("app"));
        assert!(position("lib") < position("app"));
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn build_order_is_deterministic_across_runs() {
        let g = graph(&[
            ("a", &[]),
            ("b", &[]),
            ("c", &["a", "b"]),
            ("d", &["a"]),
            ("e", &["c", "d"]),
        ]);
        let first = g.build_order().expect("acyclic");
        for _ in 0..8 {
            assert_eq!(first, g.build_order().expect("acyclic"));
        }
    }

    #[test]
    fn independent_modules_order_by_id_not_insertion() {
        let forward = graph(&[("alpha", &[]), ("beta", &[]), ("gamma", &[])]);
        let reverse = graph(&[("gamma", &[]), ("beta", &[]), ("alpha", &[])]);
        assert_eq!(
            forward.build_order().expect("acyclic"),
            reverse.build_order().expect("acyclic"),
            "insertion order must not change the plan"
        );
    }

    #[test]
    fn a_cycle_is_an_error_not_a_partial_order() {
        let g = graph(&[("a", &["b"]), ("b", &["a"]), ("free", &[])]);
        match g.build_order() {
            Err(GraphError::Cycle { cycle }) => {
                assert_eq!(
                    cycle,
                    vec![ModuleId::new("a"), ModuleId::new("b")],
                    "the cycle members are reported, sorted"
                );
            }
            other => panic!("expected a cycle error, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_dependency_is_rejected() {
        let g = graph(&[("app", &["missing"])]);
        assert_eq!(
            g.build_order(),
            Err(GraphError::UnknownDependency {
                module: ModuleId::new("app"),
                dependency: ModuleId::new("missing"),
            })
        );
    }

    #[test]
    fn duplicate_ids_are_rejected_rather_than_overwritten() {
        let mut g = ModuleGraph::new();
        g.insert(module("app", &[])).expect("first insert");
        assert_eq!(
            g.insert(module("app", &[])),
            Err(GraphError::DuplicateModule {
                module: ModuleId::new("app")
            })
        );
    }

    #[test]
    fn duplicate_dependency_edges_are_one_ordering_constraint() {
        let g = graph(&[("app", &["lib", "lib"]), ("lib", &[])]);
        assert_eq!(
            g.direct_dependencies(&ModuleId::new("app")),
            &[ModuleId::new("lib")],
            "the graph boundary canonicalizes repeated provider edges"
        );
        assert_eq!(
            g.build_order().expect("a duplicate edge is not a cycle"),
            vec![ModuleId::new("lib"), ModuleId::new("app")]
        );
    }

    #[test]
    fn duplicate_and_nested_output_ownership_are_rejected_exactly() {
        let mut duplicate = ModuleGraph::new();
        let mut a = module("a", &[]);
        a.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/out/shared".into(),
        )];
        let mut b = module("b", &[]);
        b.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/out/shared".into(),
        )];
        duplicate.insert(a).expect("a");
        duplicate.insert(b).expect("b");
        assert_eq!(
            duplicate.build_order(),
            Err(GraphError::OverlappingOutputs {
                first_module: ModuleId::new("a"),
                first_output: PathBuf::from("/out/shared"),
                second_module: ModuleId::new("b"),
                second_output: PathBuf::from("/out/shared"),
            })
        );

        let mut nested = ModuleGraph::new();
        let mut parent = module("parent", &[]);
        parent.outputs = vec![crate::model::ModuleOutput::ClassDirectory("/out".into())];
        let mut child = module("child", &[]);
        child.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/out/classes".into(),
        )];
        nested.insert(parent).expect("parent");
        nested.insert(child).expect("child");
        assert_eq!(
            nested.build_order(),
            Err(GraphError::OverlappingOutputs {
                first_module: ModuleId::new("child"),
                first_output: PathBuf::from("/out/classes"),
                second_module: ModuleId::new("parent"),
                second_output: PathBuf::from("/out"),
            })
        );
    }

    #[test]
    fn filesystem_roots_and_output_input_overlap_are_rejected_exactly() {
        let mut root_graph = ModuleGraph::new();
        let mut root = module("root", &[]);
        root.outputs = vec![crate::model::ModuleOutput::ClassDirectory("/".into())];
        root_graph.insert(root).expect("root module");
        assert_eq!(
            root_graph.build_order(),
            Err(GraphError::FilesystemRootOutput {
                module: ModuleId::new("root"),
                output: PathBuf::from("/"),
            })
        );

        let mut nested_graph = ModuleGraph::new();
        let mut producer = module("producer", &[]);
        producer.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/repo/consumer/src/generated".into(),
        )];
        let mut consumer = module("consumer", &[]);
        consumer.source_roots.push(crate::model::SourceRoot {
            path: PathBuf::from("/repo/consumer/src"),
            kind: crate::model::SourceRootKind::Main,
            generated: false,
        });
        nested_graph.insert(producer).expect("producer");
        nested_graph.insert(consumer).expect("consumer");
        assert_eq!(
            nested_graph.build_order(),
            Err(GraphError::OutputOverlapsInput {
                output_module: ModuleId::new("producer"),
                output: PathBuf::from("/repo/consumer/src/generated"),
                input_module: ModuleId::new("consumer"),
                input: PathBuf::from("/repo/consumer/src"),
            })
        );

        let mut ordinary = ModuleGraph::new();
        let mut app = module("app", &[]);
        app.source_roots.push(crate::model::SourceRoot {
            path: PathBuf::from("/repo/app/src"),
            kind: crate::model::SourceRootKind::Main,
            generated: false,
        });
        app.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/repo/app/build/classes".into(),
        )];
        ordinary.insert(app).expect("app");
        assert_eq!(ordinary.build_order(), Ok(vec![ModuleId::new("app")]));
    }

    #[test]
    fn an_owned_friend_output_requires_its_owner_as_a_dependency() {
        let mut graph = ModuleGraph::new();
        let mut main = module("main", &[]);
        main.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/out/main".into(),
        )];
        let mut test = module("test", &[]);
        test.friend_paths = vec![PathBuf::from("/out/main")];
        graph.insert(main).expect("main");
        graph.insert(test).expect("test");
        assert_eq!(
            graph.build_order(),
            Err(GraphError::FriendOwnerNotDependency {
                module: ModuleId::new("test"),
                friend_path: PathBuf::from("/out/main"),
                owner: ModuleId::new("main"),
            })
        );

        let mut valid = ModuleGraph::new();
        let mut main = module("main", &[]);
        main.outputs = vec![crate::model::ModuleOutput::ClassDirectory(
            "/out/main".into(),
        )];
        let mut test = module("test", &["main"]);
        test.friend_paths = vec![PathBuf::from("/out/main")];
        valid.insert(main).expect("main");
        valid.insert(test).expect("test");
        assert_eq!(
            valid.build_order().expect("friend owner is ordered"),
            vec![ModuleId::new("main"), ModuleId::new("test")]
        );
    }

    #[test]
    fn transitive_dependents_is_what_a_changed_abi_invalidates() {
        //  core <- lib <- app
        //       <- tool
        let g = graph(&[
            ("core", &[]),
            ("lib", &["core"]),
            ("app", &["lib"]),
            ("tool", &["core"]),
            ("unrelated", &[]),
        ]);
        assert_eq!(
            g.transitive_dependents(&ModuleId::new("core")),
            vec![
                ModuleId::new("app"),
                ModuleId::new("lib"),
                ModuleId::new("tool")
            ],
            "a core ABI change rebuilds everything downstream, and nothing else"
        );
        assert_eq!(
            g.transitive_dependents(&ModuleId::new("lib")),
            vec![ModuleId::new("app")]
        );
        assert!(g
            .transitive_dependents(&ModuleId::new("unrelated"))
            .is_empty());
    }

    #[test]
    fn transitive_dependents_terminates_on_a_cycle() {
        let g = graph(&[("a", &["b"]), ("b", &["a"]), ("c", &["a"])]);
        assert_eq!(
            g.transitive_dependents(&ModuleId::new("a")),
            vec![ModuleId::new("b"), ModuleId::new("c")],
            "a cycle must not hang the invalidation query"
        );
    }

    #[test]
    fn a_module_without_an_id_is_rejected() {
        let mut g = ModuleGraph::new();
        let orphan = Module {
            display_name: "anonymous".into(),
            ..Module::default()
        };
        assert_eq!(
            g.insert(orphan),
            Err(GraphError::MissingId {
                display_name: "anonymous".into()
            })
        );
    }
}
