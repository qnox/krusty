//! `show dependencies`: each selected module's resolved dependency graphs, main and then, with
//! tests, test, each for the compile and the runtime classpath.

use crate::configuration::Configuration;
use crate::diagnostic::Diagnostics;
use crate::maven::{Metadata, Store};
use crate::model::Model;
use crate::module::ModuleHeader;
use crate::resolution;
use crate::show;

/// The dependencies of the `shown` modules of `model` (configured as `configured`), as the
/// toolchain prints them. Every artifact in a graph must be read completely: why one could not be
/// goes to `diagnostics` as an error, module by module in the project's order and, within a
/// module, in the order its graphs meet the artifacts. The toolchain would download what is
/// missing and reports none of these problems here; krusty-toolchain refuses instead of printing
/// a graph it could not complete.
pub fn show(
    model: &Model,
    configured: &[Configuration],
    shown: impl Fn(&ModuleHeader) -> bool,
    include_tests: bool,
    diagnostics: &mut Diagnostics,
) -> Result<String, String> {
    let declarations = resolution::read_declarations(&model.modules, configured)?;
    let root = Store::default_root()
        .ok_or("cannot locate the user cache directory: set KOTLIN_SHARED_CACHE_DIR")?;
    // The local Maven repository is read, before the cache, when any module lists it.
    let local = declarations
        .iter()
        .any(|module| module.maven_local)
        .then(Store::local_repository)
        .flatten();
    let store = match local {
        Some(local) => Store::with_local(&local, &root),
        None => Store::new(&root),
    };
    let metadata = Metadata::new(&store);
    let mut resolvers = resolution::Resolvers::new(&metadata);
    let mut output = String::new();
    let mut problems = Vec::new();
    for (index, module) in model.modules.iter().enumerate() {
        if !shown(module) {
            continue;
        }
        let (mut graphs, resolver) = resolvers.resolve_module(&declarations, index, include_tests);
        for graph in &mut graphs {
            resolution::collect_problems(graph, resolver, &mut problems);
        }
        output.push_str(&show::module_dependencies(
            &module.name,
            &mut graphs,
            resolver,
        ));
    }
    for problem in &problems {
        diagnostics.push(problem.diagnostic());
    }
    Ok(output)
}
