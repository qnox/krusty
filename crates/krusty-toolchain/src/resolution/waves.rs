//! Resolving a graph (`Resolver.buildGraph`): waves of breadth-first resolution. Within a wave,
//! nodes that request different versions of one `group:module` are a conflict and are not
//! expanded; between waves the conflicts are resolved — every node aligns on the highest version —
//! versions BOMs manage are given to dependencies declared without one, and the nodes that changed
//! are resolved again.

use std::collections::{HashMap, HashSet, VecDeque};

use super::bom_versions::bom_version;
use super::graph::{Graph, Key, Kind, NodeId};
use crate::maven::{ArtifactResolver, ComparableVersion};

/// The nodes conflict resolution knows (`ConflictResolver`), by key.
#[derive(Default)]
struct Registry {
    similar: HashMap<Key, Vec<NodeId>>,
    registered: HashSet<NodeId>,
    /// The conflicting keys, in the order they were found.
    conflicted: Vec<Key>,
    /// Nodes requested without a version, waiting for a BOM to give one.
    unversioned: Vec<NodeId>,
}

/// Whether `versions` hold more than one distinct version (`seesConflictsIn`).
fn conflicting(versions: &[String]) -> bool {
    if versions.iter().all(|version| *version == versions[0]) {
        return false;
    }
    let mut distinct: Vec<ComparableVersion> = Vec::new();
    for version in versions {
        let version = ComparableVersion::new(version);
        if !distinct.contains(&version) {
            distinct.push(version);
        }
    }
    distinct.len() > 1
}

impl Registry {
    fn versions(&self, graph: &Graph, key: Key) -> Vec<String> {
        self.similar[&key]
            .iter()
            .filter_map(|&id| graph.node(id).resolved_version())
            .collect()
    }

    /// Register `id` and tell whether it conflicts (`registerAndDetectConflicts`).
    fn register(&mut self, graph: &Graph, id: NodeId) -> bool {
        let new = self.registered.insert(id);
        let node = graph.node(id);
        let Some(key) = node.key() else {
            return false;
        };
        // A node is among its key's similar nodes exactly while it is registered.
        let similar = self.similar.entry(key).or_default();
        if new {
            similar.push(id);
        }
        if matches!(node.kind, Kind::Maven(_))
            && node.resolved_version().is_none()
            && node.original_version().is_none()
            && !self.unversioned.contains(&id)
        {
            self.unversioned.push(id);
        }
        if self.conflicted.contains(&key) {
            return true;
        }
        if self.similar[&key].len() > 1 && conflicting(&self.versions(graph, key)) {
            self.conflicted.push(key);
            return true;
        }
        false
    }

    /// Register a node and, unless it was registered, its whole subgraph
    /// (`registerAndDetectConflictsWithChildren`).
    fn register_with_children(
        &mut self,
        graph: &mut Graph,
        resolver: &ArtifactResolver<'_>,
        id: NodeId,
    ) {
        if self.registered.contains(&id) {
            return;
        }
        for node in graph.breadth_first(id, resolver, |_, _| true) {
            self.register(graph, node);
        }
    }

    /// Align the nodes of `key` on their highest version (`HighestVersionStrategy`).
    fn resolve_conflict(&self, graph: &mut Graph, resolver: &mut ArtifactResolver<'_>, key: Key) {
        let versions = self.versions(graph, key);
        if !conflicting(&versions) {
            return;
        }
        let mut highest: Option<(ComparableVersion, &String)> = None;
        for version in &versions {
            let comparable = ComparableVersion::new(version);
            if highest.as_ref().is_none_or(|(best, _)| *best < comparable) {
                highest = Some((comparable, version));
            }
        }
        let Some((_, highest)) = highest else { return };
        let highest = highest.clone();
        for &id in &self.similar[&key] {
            if graph.node(id).original_version().is_none() {
                // Waits for a BOM to give it a version first.
                continue;
            }
            match graph.node(id).kind {
                Kind::Maven(_) => graph.set_maven_version(id, &highest, resolver),
                Kind::Constraint(_) => graph.set_constraint_version(id, &highest),
                _ => {}
            }
        }
    }

    /// Give dependencies declared without a version the version their BOMs manage
    /// (`UnspecifiedMavenDependencyVersionHelper.resolveVersions`).
    fn versions_from_boms(
        &mut self,
        graph: &mut Graph,
        resolver: &mut ArtifactResolver<'_>,
    ) -> Vec<NodeId> {
        let pending: Vec<NodeId> = self
            .unversioned
            .iter()
            .copied()
            .filter(|&id| graph.node(id).original_version().is_none())
            .collect();
        let found: Vec<(NodeId, String)> = pending
            .into_iter()
            .filter_map(|id| Some((id, bom_version(graph, resolver, id)?)))
            .collect();
        for (id, version) in &found {
            graph.set_bom_version(*id, version, resolver);
            self.unversioned.retain(|known| known != id);
        }
        found.into_iter().map(|(id, _)| id).collect()
    }

    /// Resolve the wave's conflicts and return the nodes to resolve again (`resolveConflicts`).
    fn resolve_conflicts(
        &mut self,
        graph: &mut Graph,
        resolver: &mut ArtifactResolver<'_>,
        root: NodeId,
    ) -> Vec<NodeId> {
        let from_boms = self.versions_from_boms(graph, resolver);
        for &id in &from_boms {
            self.register(graph, id);
        }
        let mut again = Vec::new();
        for key in std::mem::take(&mut self.conflicted) {
            self.resolve_conflict(graph, resolver, key);
            again.extend(self.similar[&key].iter().copied());
        }
        again.extend(from_boms);
        again.extend(self.reconcile(graph, resolver, root));
        let mut seen = HashSet::new();
        again.retain(|id| seen.insert(*id));
        again
    }

    /// Forget the nodes no longer in the graph and register the ones back in it
    /// (`reconcileRegistryWithGraph`); the latter are returned.
    fn reconcile(
        &mut self,
        graph: &mut Graph,
        resolver: &ArtifactResolver<'_>,
        root: NodeId,
    ) -> Vec<NodeId> {
        let order = graph.breadth_first(root, resolver, |_, _| true);
        let in_graph: HashSet<NodeId> = order.iter().copied().collect();
        for similar in self.similar.values_mut() {
            similar.retain(|id| in_graph.contains(id));
        }
        self.registered.retain(|id| in_graph.contains(id));
        self.unversioned.retain(|id| in_graph.contains(id));
        let mut back = Vec::new();
        for id in order {
            if !self.registered.contains(&id) {
                self.register(graph, id);
                back.push(id);
            }
        }
        back
    }
}

/// Resolve the graph below `root`, reading what artifacts declare through `resolver`.
pub fn resolve(graph: &mut Graph, resolver: &mut ArtifactResolver<'_>, root: NodeId) {
    let mut registry = Registry::default();
    let mut resolved: HashSet<NodeId> = HashSet::new();
    let mut wave = vec![root];
    while !wave.is_empty() {
        let mut queue: VecDeque<NodeId> = wave.into();
        while let Some(id) = queue.pop_front() {
            if resolved.contains(&id) {
                registry.register_with_children(graph, resolver, id);
                continue;
            }
            if registry.register(graph, id) {
                continue;
            }
            if let Kind::Maven(maven) = &graph.node(id).kind {
                if maven.current.version.is_some() {
                    resolver.declared(maven.current_artifact(), graph.node(id).scope);
                }
            }
            queue.extend(graph.children(id, resolver));
            resolved.insert(id);
        }
        wave = registry.resolve_conflicts(graph, resolver, root);
        let again: HashSet<NodeId> = wave.iter().copied().collect();
        resolved.retain(|id| !again.contains(id));
    }
}
