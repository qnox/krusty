//! Dependency resolution (`dependency-resolution`, `frontend/dr`): each module's declared
//! dependencies, the graph of the artifacts they lead to, with every `group:module` aligned on one
//! version per graph.

mod bom_versions;
mod declarations;
mod graph;
mod problems;
mod roots;
mod waves;

pub use declarations::{read as read_declarations, ModuleDeclarations};
pub use graph::{Graph, Key, Kind, NodeId};
pub use problems::collect as collect_problems;
pub use roots::ModuleGraph;

use crate::maven::{ArtifactResolver, Metadata};

/// The resolvers of a project's modules: one for each JDK version and `excludeDependencies` list,
/// so that what an artifact declares is read once for every module that reads it alike.
pub struct Resolvers<'m> {
    metadata: &'m Metadata<'m>,
    resolvers: Vec<ArtifactResolver<'m>>,
}

impl<'m> Resolvers<'m> {
    pub fn new(metadata: &'m Metadata<'m>) -> Self {
        Self {
            metadata,
            resolvers: Vec::new(),
        }
    }

    /// The resolved main graph of `modules[index]` and, with `tests`, its test graph, with the
    /// resolver that holds what the graphs' artifacts declare.
    pub fn resolve_module(
        &mut self,
        modules: &[ModuleDeclarations],
        index: usize,
        tests: bool,
    ) -> (Vec<ModuleGraph>, &ArtifactResolver<'m>) {
        let module = &modules[index];
        let position = match self
            .resolvers
            .iter()
            .position(|resolver| resolver.serves(&module.jdk_version, &module.blocklist))
        {
            Some(position) => position,
            None => {
                self.resolvers.push(ArtifactResolver::new(
                    self.metadata,
                    module.jdk_version.clone(),
                    module.blocklist.clone(),
                ));
                self.resolvers.len() - 1
            }
        };
        let resolver = &mut self.resolvers[position];
        resolver.forget_reads();
        let fragments: &[bool] = if tests { &[false, true] } else { &[false] };
        let graphs = fragments
            .iter()
            .map(|&test| {
                let mut graph = roots::module_graph(modules, index, test, resolver);
                waves::resolve(&mut graph.graph, resolver, graph.root);
                graph
            })
            .collect();
        (graphs, resolver)
    }
}
