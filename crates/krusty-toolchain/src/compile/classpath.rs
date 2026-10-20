//! A module's compile classpath, in the toolchain's order: the compiled output of the modules its
//! resolved compile graph holds, then the files of its Maven artifacts, each in the order a
//! breadth-first walk of the graph meets them. (The planner puts each module's outputs in its place:
//! its compiled output when it has Kotlin sources, then its resources output when it has
//! resources.) An artifact contributes the files its metadata publishes for compilation; one that
//! only points at another module (a multiplatform library's root), a BOM and a POM-packaged
//! artifact contribute none.

use std::path::PathBuf;

use crate::maven::{missing, ArtifactResolver, Problem, Store};
use crate::resolution::{Kind, ModuleGraph, NodeId};

/// What a module's compile classpath holds.
pub(super) struct Classpath {
    /// The modules whose compiled output is on the classpath, by name, in classpath order.
    pub modules: Vec<String>,
    /// The artifact files, in classpath order, each once.
    pub files: Vec<PathBuf>,
}

/// The compile classpath of the module `graph` is resolved for. Why an artifact could not be read
/// completely, or a file of it is in no repository, is a problem appended to `problems` unless it
/// is already there; the toolchain would download what is missing.
pub(super) fn classpath(
    graph: &mut ModuleGraph,
    resolver: &ArtifactResolver<'_>,
    store: &Store,
    problems: &mut Vec<Problem>,
) -> Classpath {
    let compile = graph.modules[0];
    walk(graph, compile, resolver, store, problems)
}

/// The artifact files of the runtime classpath of a tool `graph` is resolved for
/// (`Resolvers::resolve_artifacts`), in the order a breadth-first walk meets them, each once.
/// Problems are reported as for [`classpath`].
pub(super) fn runtime_classpath(
    graph: &mut ModuleGraph,
    resolver: &ArtifactResolver<'_>,
    store: &Store,
    problems: &mut Vec<Problem>,
) -> Vec<PathBuf> {
    let runtime = graph.modules[1];
    walk(graph, runtime, resolver, store, problems).files
}

fn walk(
    graph: &mut ModuleGraph,
    start: NodeId,
    resolver: &ArtifactResolver<'_>,
    store: &Store,
    problems: &mut Vec<Problem>,
) -> Classpath {
    let mut classpath = Classpath {
        modules: Vec::new(),
        files: Vec::new(),
    };
    let mut report = |problem: Problem| {
        if !problems.contains(&problem) {
            problems.push(problem);
        }
    };
    for id in graph.graph.breadth_first(start, resolver, |_, _| true) {
        let node = graph.graph.node(id);
        let maven = match &node.kind {
            Kind::Module(module) if !module.top_level => {
                classpath.modules.push(module.name.clone());
                continue;
            }
            Kind::Maven(maven) => maven,
            _ => continue,
        };
        let coordinates = &resolver.artifact(maven.current_artifact()).coordinates;
        if coordinates.version.is_none() {
            report(Problem::general(format!(
                "{} has no version: no BOM or constraint gives it one, so it names no file to compile against",
                coordinates.pretty(None)
            )));
            continue;
        }
        let Some(declared) = resolver.read_declared(maven.current_artifact(), node.scope) else {
            report(Problem::general(format!(
                "{} is in the resolved graph, but its metadata was not read",
                coordinates.pretty(None)
            )));
            continue;
        };
        for problem in &declared.problems {
            report(problem.clone());
        }
        if maven.requested.is_bom {
            continue;
        }
        for file in &declared.files {
            let located =
                file.clone()
                    .and_then(|name| match store.artifact_file(coordinates, &name)? {
                        Some(path) => Ok(path),
                        None => Err(missing(coordinates, &format!("file `{name}`"))),
                    });
            match located {
                Ok(path) => {
                    if !classpath.files.contains(&path) {
                        classpath.files.push(path);
                    }
                }
                Err(problem) => report(problem),
            }
        }
    }
    classpath
}
