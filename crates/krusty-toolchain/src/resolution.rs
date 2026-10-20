//! Dependency resolution (`dependency-resolution`, `frontend/dr`): each module's declared
//! dependencies, the graph of the artifacts they lead to, with every `group:module` aligned on one
//! version per graph.

mod bom_versions;
mod declarations;
mod graph;
mod problems;
mod roots;
mod waves;

pub use declarations::{
    maven_artifact, read as read_declarations, Declaration, ModuleDeclarations, Target,
};
pub use graph::{Graph, Key, Kind, NodeId};
pub use problems::collect as collect_problems;
pub use roots::ModuleGraph;

use crate::maven::{ArtifactResolver, Coordinates, Metadata};

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
        let resolver = self.resolver_for(&modules[index]);
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

    /// The resolved graph of `artifacts` as a tool of `module` runs them (KSP and its processors):
    /// a module of their own that depends on them at runtime only, resolved from the repositories,
    /// for the JDK and with the `excludeDependencies` of `module`. Its runtime module node (the
    /// second) holds them.
    pub fn resolve_artifacts(
        &mut self,
        module: &ModuleDeclarations,
        artifacts: &[Coordinates],
    ) -> (ModuleGraph, &ArtifactResolver<'m>) {
        let tool = ModuleDeclarations {
            name: module.name.clone(),
            main: artifacts
                .iter()
                .map(|coordinates| Declaration {
                    target: Target::Maven {
                        coordinates: coordinates.clone(),
                        bom: false,
                        trace: String::new(),
                    },
                    compile: false,
                    runtime: true,
                    exported: false,
                })
                .collect(),
            test: Vec::new(),
            jdk_version: module.jdk_version.clone(),
            blocklist: module.blocklist.clone(),
            maven_local: module.maven_local,
        };
        let resolver = self.resolver_for(&tool);
        resolver.forget_reads();
        let mut graph = roots::module_graph(std::slice::from_ref(&tool), 0, false, resolver);
        waves::resolve(&mut graph.graph, resolver, graph.root);
        (graph, resolver)
    }

    /// The resolver for `module`'s JDK and `excludeDependencies`, made the first time one is
    /// needed.
    fn resolver_for(&mut self, module: &ModuleDeclarations) -> &mut ArtifactResolver<'m> {
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
        &mut self.resolvers[position]
    }
}
