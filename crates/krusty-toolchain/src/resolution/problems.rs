//! The problems of a resolved graph: why an artifact in it could not be (completely) read, for
//! each artifact the graph resolved to, in the order a breadth-first walk from its module nodes
//! meets them, each once.

use super::graph::Kind;
use super::roots::ModuleGraph;
use crate::maven::{ArtifactResolver, Problem};

/// The problems of `graph`'s artifacts, appended to `problems` unless already there.
pub fn collect(
    graph: &mut ModuleGraph,
    resolver: &ArtifactResolver<'_>,
    problems: &mut Vec<Problem>,
) {
    for module in graph.modules {
        for id in graph.graph.breadth_first(module, resolver, |_, _| true) {
            let node = graph.graph.node(id);
            let Kind::Maven(maven) = &node.kind else {
                continue;
            };
            let Some(declared) = resolver.read_declared(maven.current_artifact(), node.scope)
            else {
                continue;
            };
            for problem in &declared.problems {
                if !problems.contains(problem) {
                    problems.push(problem.clone());
                }
            }
        }
    }
}
