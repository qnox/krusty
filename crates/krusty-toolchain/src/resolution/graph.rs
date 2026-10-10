//! A dependency graph (`DependencyNode`): a root, module nodes, the holders of each declared
//! dependency, and the Maven artifacts and constraints below them. Nodes are indices into one
//! arena; an artifact requested in one version is one node wherever it is requested, and its
//! children follow the version it currently resolves to.

use std::collections::{HashMap, HashSet};

use crate::maven::{
    Artifact, ArtifactId, ArtifactResolver, Constraint, ConstraintId, Coordinates, RichVersion,
    Scope,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A `group:module` that Maven and constraint nodes conflict by, interned per graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(u32);

/// A module's dependencies in one scope (`ModuleDependencyNode`).
pub struct ModuleNode {
    pub name: String,
    pub test: bool,
    /// The module the graph is resolved for, rather than one it depends on.
    pub top_level: bool,
}

/// One declared dependency of a module's fragment (`DirectFragmentDependencyNode`).
pub struct Holder {
    pub module: String,
    pub fragment: &'static str,
    /// The coordinates as declared.
    pub declared: Coordinates,
    /// Where the declaration comes from, as printed after it (`, implicit`).
    pub trace: String,
    /// Declared by a module the graph's module depends on.
    pub transitive: bool,
}

/// A Maven artifact as requested, and the version it resolves to.
pub struct MavenNode {
    pub requested: Artifact,
    /// The requested coordinates with the version conflict resolution or a BOM settled on.
    pub current: Coordinates,
    /// The artifact of `current`.
    current_id: ArtifactId,
    /// The version a BOM gave a dependency declared without one.
    pub from_bom: Option<String>,
}

impl MavenNode {
    pub fn current_artifact(&self) -> ArtifactId {
        self.current_id
    }

    /// The version requested, or the one a BOM gave (`originalVersion`).
    pub fn original_version(&self) -> Option<&str> {
        self.requested
            .coordinates
            .version
            .as_deref()
            .or(self.from_bom.as_deref())
    }
}

/// A version constraint, and the version conflict resolution aligned it to.
pub struct ConstraintNode {
    pub requested: Constraint,
    pub current: RichVersion,
}

pub enum Kind {
    Root,
    Module(ModuleNode),
    Holder(Holder),
    Maven(MavenNode),
    Constraint(ConstraintNode),
}

/// What a Maven node's children were last computed from: the artifact, and whether its metadata
/// had been read.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Source {
    artifact: ArtifactId,
    read: bool,
}

pub struct Node {
    pub kind: Kind,
    pub scope: Scope,
    key: Option<Key>,
    children: Vec<NodeId>,
    source: Option<Source>,
    parents: Vec<NodeId>,
}

impl Node {
    /// The key Maven and constraint nodes conflict by (`group:module`); other nodes have none.
    pub fn key(&self) -> Option<Key> {
        self.key
    }

    /// The version the node currently resolves to (`resolvedVersion`).
    pub fn resolved_version(&self) -> Option<String> {
        match &self.kind {
            Kind::Maven(maven) => maven.current.version.clone(),
            Kind::Constraint(constraint) => constraint.current.resolve(),
            _ => None,
        }
    }

    /// The version requested (`originalVersion`).
    pub fn original_version(&self) -> Option<String> {
        match &self.kind {
            Kind::Maven(maven) => maven.original_version().map(str::to_string),
            Kind::Constraint(constraint) => constraint.requested.version.resolve(),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct Graph {
    nodes: Vec<Node>,
    keys: HashMap<(String, String), Key>,
    /// Maven and constraint nodes by what they request, for the compile and the runtime scope.
    maven: [HashMap<ArtifactId, NodeId>; 2],
    constraints: [HashMap<ConstraintId, NodeId>; 2],
}

fn scope_index(scope: Scope) -> usize {
    match scope {
        Scope::Compile => 0,
        Scope::Runtime => 1,
    }
}

impl Graph {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    fn intern(&mut self, group: &str, module: &str) -> Key {
        let next = Key(self.keys.len() as u32);
        *self
            .keys
            .entry((group.to_string(), module.to_string()))
            .or_insert(next)
    }

    fn push(&mut self, kind: Kind, scope: Scope) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        let key = match &kind {
            Kind::Maven(maven) => {
                let coordinates = &maven.requested.coordinates;
                Some(self.intern(&coordinates.group, &coordinates.artifact))
            }
            Kind::Constraint(constraint) => {
                let requested = &constraint.requested;
                Some(self.intern(&requested.group, &requested.module))
            }
            _ => None,
        };
        self.nodes.push(Node {
            kind,
            scope,
            key,
            children: Vec::new(),
            source: None,
            parents: Vec::new(),
        });
        id
    }

    fn add_parent(&mut self, child: NodeId, parent: NodeId) {
        let parents = &mut self.node_mut(child).parents;
        if !parents.contains(&parent) {
            parents.push(parent);
        }
    }

    fn remove_parent(&mut self, child: NodeId, parent: NodeId) {
        self.node_mut(child)
            .parents
            .retain(|known| *known != parent);
    }

    /// A root, module or holder node with fixed children.
    pub fn add(&mut self, kind: Kind, scope: Scope, children: Vec<NodeId>) -> NodeId {
        let id = self.push(kind, scope);
        for &child in &children {
            self.add_parent(child, id);
        }
        self.node_mut(id).children = children;
        id
    }

    /// The node requesting `artifact` in `scope`, created on first request.
    pub fn maven(
        &mut self,
        artifact: ArtifactId,
        scope: Scope,
        resolver: &ArtifactResolver<'_>,
    ) -> NodeId {
        if let Some(&id) = self.maven[scope_index(scope)].get(&artifact) {
            return id;
        }
        let requested = resolver.artifact(artifact);
        let id = self.push(
            Kind::Maven(MavenNode {
                requested: requested.clone(),
                current: requested.coordinates.clone(),
                current_id: artifact,
                from_bom: None,
            }),
            scope,
        );
        self.maven[scope_index(scope)].insert(artifact, id);
        id
    }

    fn constraint(
        &mut self,
        constraint: ConstraintId,
        scope: Scope,
        resolver: &ArtifactResolver<'_>,
    ) -> NodeId {
        if let Some(&id) = self.constraints[scope_index(scope)].get(&constraint) {
            return id;
        }
        let requested = resolver.constraint(constraint);
        let id = self.push(
            Kind::Constraint(ConstraintNode {
                requested: requested.clone(),
                current: requested.version.clone(),
            }),
            scope,
        );
        self.constraints[scope_index(scope)].insert(constraint, id);
        id
    }

    pub fn parents(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).parents
    }

    /// Whether a node above `id`, through any parent it has, is keyed `key`.
    fn has_ancestor_keyed(&self, id: NodeId, key: Option<Key>) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut pending = vec![id];
        while let Some(next) = pending.pop() {
            for &parent in self.parents(next) {
                if !seen[parent.index()] {
                    if self.node(parent).key == key {
                        return true;
                    }
                    seen[parent.index()] = true;
                    pending.push(parent);
                }
            }
        }
        false
    }

    /// The keys of `id` and every node above it.
    fn ancestor_keys(&self, id: NodeId) -> HashSet<Key> {
        let mut seen = vec![false; self.nodes.len()];
        let mut keys: HashSet<Key> = self.node(id).key.into_iter().collect();
        let mut pending = vec![id];
        while let Some(next) = pending.pop() {
            for &parent in self.parents(next) {
                if !seen[parent.index()] {
                    seen[parent.index()] = true;
                    keys.extend(self.node(parent).key);
                    pending.push(parent);
                }
            }
        }
        keys
    }

    /// The node's children, recomputed for a Maven node whose resolved artifact or its metadata
    /// changed since they were last computed.
    pub fn children(&mut self, id: NodeId, resolver: &ArtifactResolver<'_>) -> Vec<NodeId> {
        let node = self.node(id);
        if let Kind::Maven(maven) = &node.kind {
            let source = Source {
                read: resolver.is_read(maven.current_id, node.scope),
                artifact: maven.current_id,
            };
            if node.source != Some(source) {
                self.recompute_children(id, source, resolver);
            }
        }
        self.node(id).children.clone()
    }

    /// The children the node had when last computed, without recomputing them.
    pub fn known_children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    fn recompute_children(&mut self, id: NodeId, source: Source, resolver: &ArtifactResolver<'_>) {
        let scope = self.node(id).scope;
        let declared = resolver.read_declared(source.artifact, scope);
        let closure = self.ancestor_keys(id);
        let mut children = Vec::new();
        if let Some(declared) = &declared {
            for &artifact in &declared.children {
                let child = self.maven(artifact, scope, resolver);
                self.add_parent(child, id);
                if self.forms_cycle(child, &closure) {
                    self.remove_parent(child, id);
                } else {
                    children.push(child);
                }
            }
            for &constraint in &declared.constraints {
                let child = self.constraint(constraint, scope, resolver);
                self.add_parent(child, id);
                children.push(child);
            }
        }
        let old = std::mem::take(&mut self.node_mut(id).children);
        for stale in old.into_iter().filter(|child| !children.contains(child)) {
            self.remove_parent(stale, id);
        }
        let node = self.node_mut(id);
        node.children = children;
        node.source = Some(source);
    }

    /// Whether `child` would depend on its own `group:module` (`formsCycle`): with one parent, judged
    /// by the keys of the parent and its ancestors; with several, by all of its own ancestors.
    fn forms_cycle(&self, child: NodeId, closure: &HashSet<Key>) -> bool {
        let key = self.node(child).key;
        if self.parents(child).len() == 1 {
            key.is_some_and(|key| closure.contains(&key))
        } else {
            self.has_ancestor_keyed(child, key)
        }
    }

    /// Point a Maven node at `version` (`updateDependency`), recomputing its children.
    pub fn set_maven_version(
        &mut self,
        id: NodeId,
        version: &str,
        resolver: &mut ArtifactResolver<'_>,
    ) {
        if let Kind::Maven(maven) = &mut self.node_mut(id).kind {
            maven.current = maven.current.with_version(Some(version));
            maven.current_id = resolver.intern(&Artifact {
                coordinates: maven.current.clone(),
                is_bom: maven.requested.is_bom,
            });
        }
        self.children(id, resolver);
    }

    /// Give a node requested without a version the version a BOM manages.
    pub fn set_bom_version(
        &mut self,
        id: NodeId,
        version: &str,
        resolver: &mut ArtifactResolver<'_>,
    ) {
        if let Kind::Maven(maven) = &mut self.node_mut(id).kind {
            maven.from_bom = Some(version.to_string());
        }
        self.set_maven_version(id, version, resolver);
    }

    /// Align a constraint to `version`.
    pub fn set_constraint_version(&mut self, id: NodeId, version: &str) {
        if let Kind::Constraint(constraint) = &mut self.node_mut(id).kind {
            constraint.current = RichVersion::requires(version);
        }
    }

    /// Distinct nodes breadth-first from `start` (`distinctBfsSequence`), descending only into
    /// children `descend` admits.
    pub fn breadth_first(
        &mut self,
        start: NodeId,
        resolver: &ArtifactResolver<'_>,
        descend: impl Fn(&Graph, NodeId) -> bool,
    ) -> Vec<NodeId> {
        let mut order = Vec::new();
        let mut visited = vec![false; self.nodes.len()];
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(next) = queue.pop_front() {
            if next.index() >= visited.len() {
                visited.resize(self.nodes.len(), false);
            }
            if visited[next.index()] {
                continue;
            }
            visited[next.index()] = true;
            order.push(next);
            for child in self.children(next, resolver) {
                if visited.get(child.index()) != Some(&true) && descend(self, child) {
                    queue.push_back(child);
                }
            }
        }
        order
    }
}
