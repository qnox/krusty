//! The version a BOM gives a dependency declared without one
//! (`DirectMavenDependencyUnspecifiedVersionResolver`): the BOMs declared by the module whose
//! declaration leads to the dependency, nested BOMs included, in order; the first that constrains
//! the dependency's `group:module` decides.

use std::collections::HashSet;

use super::graph::{Graph, Kind, NodeId};
use crate::maven::ArtifactResolver;

/// The holders above `id`: its direct parents if any of them is one, else the nearest on every
/// path up (`fragmentDependencies`).
fn holders(graph: &Graph, id: NodeId) -> Vec<NodeId> {
    let direct: Vec<NodeId> = graph
        .parents(id)
        .iter()
        .copied()
        .filter(|&parent| matches!(graph.node(parent).kind, Kind::Holder(_)))
        .collect();
    if !direct.is_empty() {
        return direct;
    }
    let mut found = Vec::new();
    let mut visited = HashSet::new();
    nearest_holders(graph, id, &mut visited, &mut found);
    found
}

fn nearest_holders(
    graph: &Graph,
    id: NodeId,
    visited: &mut HashSet<NodeId>,
    found: &mut Vec<NodeId>,
) {
    if !visited.insert(id) {
        return;
    }
    if matches!(graph.node(id).kind, Kind::Holder(_)) {
        if !found.contains(&id) {
            found.push(id);
        }
        return;
    }
    for &parent in graph.parents(id) {
        nearest_holders(graph, parent, visited, found);
    }
}

fn is_bom(graph: &Graph, id: NodeId) -> bool {
    matches!(&graph.node(id).kind, Kind::Maven(maven) if maven.requested.is_bom)
}

/// The BOMs among `nodes`, each followed by the BOMs it depends on (`filterBomDependencies`).
fn with_nested_boms(
    graph: &mut Graph,
    resolver: &ArtifactResolver<'_>,
    nodes: Vec<NodeId>,
) -> Vec<NodeId> {
    let mut boms = Vec::new();
    let declared: Vec<NodeId> = nodes
        .into_iter()
        .filter(|&node| is_bom(graph, node))
        .collect();
    for node in declared {
        boms.extend(graph.breadth_first(node, resolver, is_bom));
    }
    boms
}

/// The BOMs whose constraints may give `id` its version (`getBomNodes`).
fn boms(graph: &mut Graph, resolver: &ArtifactResolver<'_>, id: NodeId) -> Vec<NodeId> {
    let holders = holders(graph, id);
    let mut boms = Vec::new();
    if holders.is_empty() {
        for parent in graph.parents(id).to_vec() {
            let siblings: Vec<NodeId> = graph
                .children(parent, resolver)
                .into_iter()
                .filter(|&sibling| matches!(graph.node(sibling).kind, Kind::Maven(_)))
                .collect();
            boms.extend(with_nested_boms(graph, resolver, siblings));
        }
        return boms;
    }
    for holder in holders {
        let [module] = graph.parents(holder) else {
            continue;
        };
        let module = *module;
        if !matches!(graph.node(module).kind, Kind::Module(_)) {
            continue;
        }
        let declared: Vec<NodeId> = graph
            .known_children(module)
            .iter()
            .filter(|&&child| matches!(graph.node(child).kind, Kind::Holder(_)))
            .flat_map(|&child| graph.known_children(child).to_vec())
            .collect();
        boms.extend(with_nested_boms(graph, resolver, declared));
    }
    boms
}

/// The version the first BOM constraining `id`'s `group:module` gives it
/// (`resolveVersionFromBom`).
pub fn bom_version(
    graph: &mut Graph,
    resolver: &ArtifactResolver<'_>,
    id: NodeId,
) -> Option<String> {
    let key = graph.node(id).key();
    for bom in boms(graph, resolver, id) {
        let constraint = graph.children(bom, resolver).into_iter().find(|&child| {
            let node = graph.node(child);
            matches!(node.kind, Kind::Constraint(_)) && node.key() == key
        });
        if let Some(version) = constraint.and_then(|child| graph.node(child).original_version()) {
            return Some(version);
        }
    }
    None
}
