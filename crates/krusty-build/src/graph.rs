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
    pub fn insert(&mut self, module: Module) -> Result<(), GraphError> {
        let Some(id) = module.id.clone() else {
            return Err(GraphError::MissingId {
                display_name: module.display_name.clone(),
            });
        };
        if self.modules.contains_key(&id) {
            return Err(GraphError::DuplicateModule { module: id });
        }
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
