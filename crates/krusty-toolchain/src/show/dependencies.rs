//! `show dependencies`: each module graph printed as a tree (`DependencyNode.prettyPrint`). A node
//! printed before is marked `(*)` instead of being expanded again; constraints are printed, marked
//! `(c)`, only where they decided a version.

use std::collections::{HashMap, HashSet};

use crate::maven::{ArtifactResolver, Coordinates};
use crate::resolution::{Graph, Key, Kind, ModuleGraph, NodeId};

/// The dependencies of module `name`, as `show dependencies` prints them.
pub fn module_dependencies(
    name: &str,
    graphs: &mut [ModuleGraph],
    resolver: &ArtifactResolver<'_>,
) -> String {
    let mut out = format!("Dependencies of module {name}: \n\n");
    for module_graph in graphs.iter_mut() {
        for module in module_graph.modules {
            let mut printer = Printer::new(&mut module_graph.graph, resolver, module);
            printer.node(module, &mut String::new(), false);
            out.push_str(&printer.out);
            out.push('\n');
        }
    }
    out
}

struct Printer<'g, 'r, 's> {
    graph: &'g mut Graph,
    resolver: &'r ArtifactResolver<'s>,
    /// The Maven nodes of the printed module graph, by key.
    maven: HashMap<Key, Vec<NodeId>>,
    visited: HashSet<String>,
    out: String,
}

/// Coordinates without their packaging type, as nodes print them.
fn without_packaging(coordinates: &Coordinates) -> Coordinates {
    Coordinates {
        packaging: None,
        ..coordinates.clone()
    }
}

impl<'g, 'r, 's> Printer<'g, 'r, 's> {
    fn new(graph: &'g mut Graph, resolver: &'r ArtifactResolver<'s>, module: NodeId) -> Self {
        let mut maven: HashMap<Key, Vec<NodeId>> = HashMap::new();
        for id in graph.breadth_first(module, resolver, |_, _| true) {
            if let (Kind::Maven(_), Some(key)) = (&graph.node(id).kind, graph.node(id).key()) {
                maven.entry(key).or_default().push(id);
            }
        }
        Self {
            graph,
            resolver,
            maven,
            visited: HashSet::new(),
            out: String::new(),
        }
    }

    /// The line a node prints as (`graphEntryName`).
    fn label(&self, id: NodeId) -> String {
        let node = self.graph.node(id);
        match &node.kind {
            Kind::Root => String::new(),
            Kind::Module(module) => {
                let mut label = format!("Module {}", module.name);
                if module.top_level {
                    label.push_str(&format!(
                        "\n│ - {}\n│ - scope = {}\n│ - platforms = [jvm]",
                        if module.test { "test" } else { "main" },
                        node.scope.name()
                    ));
                }
                label
            }
            Kind::Holder(holder) => format!(
                "{}:{}:{}{}",
                holder.module,
                holder.fragment,
                holder.declared.pretty(None),
                holder.trace
            ),
            Kind::Maven(maven) => {
                let requested = without_packaging(&maven.requested.coordinates);
                if maven.current.version == requested.version {
                    without_packaging(&maven.current).pretty(None)
                } else {
                    requested.pretty(Some(
                        maven.current.version.as_deref().unwrap_or("unspecified"),
                    ))
                }
            }
            Kind::Constraint(constraint) => {
                let requested = &constraint.requested;
                if constraint.current == requested.version {
                    format!(
                        "{}:{}:{}",
                        requested.group,
                        requested.module,
                        requested
                            .version
                            .resolve()
                            .unwrap_or_else(|| "null".to_string())
                    )
                } else {
                    format!(
                        "{}:{}:{} -> {}",
                        requested.group,
                        requested.module,
                        requested.version.render(),
                        constraint.current.render()
                    )
                }
            }
        }
    }

    /// What tells a node printed before (`key` and `groupingGraphEntryKey`).
    fn identity(&self, id: NodeId) -> String {
        let node = self.graph.node(id);
        let scope = node.scope.name();
        match &node.kind {
            Kind::Root => "root".to_string(),
            Kind::Module(module) => format!("module {} {} {scope}", module.name, module.test),
            Kind::Holder(holder) => format!(
                "holder {}:{}:{}:{}{}:{scope}",
                holder.module,
                holder.fragment,
                holder.transitive,
                holder.declared.pretty(None),
                holder.trace
            ),
            Kind::Maven(maven) => format!(
                "maven {}:{}:{scope}",
                without_packaging(&maven.current).pretty(None),
                maven.requested.is_bom
            ),
            Kind::Constraint(constraint) => format!(
                "constraint {}:{}:{:?}:{scope}",
                constraint.requested.group, constraint.requested.module, constraint.current
            ),
        }
    }

    /// Whether a child is printed: anything but a constraint that decided no version
    /// (`isConstraintAffectingTheGraph`).
    fn printed(&self, id: NodeId) -> bool {
        let node = self.graph.node(id);
        let Kind::Constraint(constraint) = &node.kind else {
            return true;
        };
        let Some(dependencies) = node.key().and_then(|key| self.maven.get(&key)) else {
            return false;
        };
        let version = constraint.requested.version.resolve();
        let version = version.as_deref();
        let versions = || {
            dependencies
                .iter()
                .filter_map(|&dependency| match &self.graph.node(dependency).kind {
                    Kind::Maven(maven) => Some((
                        maven.requested.coordinates.version.as_deref(),
                        maven.current.version.as_deref(),
                    )),
                    _ => None,
                })
        };
        versions().all(|(original, current)| !(original == current && original == version))
            && versions().any(|(original, current)| version == current && original != current)
    }

    fn node(&mut self, id: NodeId, indent: &mut String, add_level: bool) {
        let label = self.label(id);
        self.out.push_str(indent);
        self.out.push_str(&label);
        let seen = !self.visited.insert(self.identity(id));
        let children = self.graph.children(id, self.resolver);
        let printed: Vec<NodeId> = children
            .iter()
            .copied()
            .filter(|&child| self.printed(child))
            .collect();
        if seen && !printed.is_empty() {
            self.out.push_str(" (*)");
        } else if matches!(self.graph.node(id).kind, Kind::Constraint(_)) {
            self.out.push_str(" (c)");
        }
        self.out.push('\n');
        if seen || children.is_empty() {
            return;
        }
        if !indent.is_empty() {
            truncate_last(indent);
            indent.push_str(if add_level { "│    " } else { "     " });
        }
        for (index, &child) in printed.iter().enumerate() {
            let another = index + 1 < printed.len();
            indent.push_str(if another {
                "├─── "
            } else {
                "╰─── "
            });
            self.node(child, indent, another);
            truncate_last(indent);
        }
    }
}

/// Drop the last indentation step (five characters).
fn truncate_last(indent: &mut String) {
    for _ in 0..5 {
        indent.pop();
    }
}
