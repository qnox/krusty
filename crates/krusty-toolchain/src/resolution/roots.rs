//! The unresolved graph of one module (`Classpath` flow): for each scope, a module node holding
//! the dependencies its fragment declares and, for each module it depends on, that module's node
//! with the dependencies that reach this classpath.

use std::collections::HashSet;

use super::declarations::{Declaration, ModuleDeclarations, Target};
use super::graph::{Graph, Holder, Kind, ModuleNode, NodeId};
use crate::maven::{Artifact, ArtifactResolver, Scope};

/// One module's graph, main or test: a root over its compile and runtime module nodes.
pub struct ModuleGraph {
    pub graph: Graph,
    pub root: NodeId,
    /// The compile, then the runtime module node.
    pub modules: [NodeId; 2],
}

/// The unresolved graph of `modules[index]`'s main or `test` fragment.
pub fn module_graph(
    modules: &[ModuleDeclarations],
    index: usize,
    test: bool,
    resolver: &mut ArtifactResolver<'_>,
) -> ModuleGraph {
    let mut graph = Graph::default();
    let mut builder = Builder {
        graph: &mut graph,
        modules,
        test,
        resolver,
    };
    let compile = builder.module(index, Scope::Compile, None, &mut HashSet::new());
    let runtime = builder.module(index, Scope::Runtime, None, &mut HashSet::new());
    let root = graph.add(Kind::Root, Scope::Compile, vec![compile, runtime]);
    ModuleGraph {
        graph,
        root,
        modules: [compile, runtime],
    }
}

struct Builder<'a, 'r> {
    graph: &'a mut Graph,
    modules: &'a [ModuleDeclarations],
    test: bool,
    resolver: &'a mut ArtifactResolver<'r>,
}

impl Builder<'_, '_> {
    /// The node of module `index` in `scope`; `dependency` is the declaration that leads to it
    /// from another module (`None` for the module the graph is for).
    fn module(
        &mut self,
        index: usize,
        scope: Scope,
        dependency: Option<&Declaration>,
        visited: &mut HashSet<usize>,
    ) -> NodeId {
        visited.insert(index);
        let direct = dependency.is_none();
        let module = &self.modules[index];
        let (declarations, fragment) = if direct && self.test {
            (&module.test, "test")
        } else {
            (&module.main, "main")
        };
        let mut children: Vec<(NodeId, bool)> = Vec::new();
        for declaration in declarations {
            let belongs = match scope {
                Scope::Compile => declaration.compile && (direct || declaration.exported),
                Scope::Runtime => declaration.runtime,
            };
            match &declaration.target {
                Target::Maven {
                    coordinates,
                    bom,
                    trace,
                } => {
                    if !*bom && !belongs {
                        continue;
                    }
                    let artifact = Artifact {
                        coordinates: coordinates.clone(),
                        is_bom: *bom,
                    };
                    let artifact = self.resolver.intern(&artifact);
                    let maven = self.graph.maven(artifact, scope, self.resolver);
                    let holder = Holder {
                        module: module.name.clone(),
                        fragment,
                        declared: coordinates.clone(),
                        trace: trace.clone(),
                        transitive: !direct,
                    };
                    let holder = self.graph.add(Kind::Holder(holder), scope, vec![maven]);
                    children.push((holder, declaration.exported && !*bom));
                }
                Target::Module(other) => {
                    if visited.contains(other) || !belongs {
                        continue;
                    }
                    let node = self.module(*other, scope, Some(declaration), visited);
                    children.push((node, declaration.exported));
                }
            }
        }
        // Exported dependencies come first.
        children.sort_by_key(|(_, exported)| !exported);
        let node = ModuleNode {
            name: module.name.clone(),
            test: self.test,
            top_level: direct,
        };
        self.graph.add(
            Kind::Module(node),
            scope,
            children.into_iter().map(|(child, _)| child).collect(),
        )
    }
}
