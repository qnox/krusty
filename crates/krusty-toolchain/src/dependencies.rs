//! `show dependencies`: each selected module's resolved dependency graphs, main and then, with
//! tests, test, each for the compile and the runtime classpath.

use crate::diagnostic::Diagnostics;
use crate::maven::{Metadata, OpenError, Store};
use crate::model::Model;
use crate::module::ModuleHeader;
use crate::resolution;
use crate::show;

/// The dependencies of the `shown` modules of `model`, as the toolchain prints them. Every
/// artifact in a graph must be read completely: why one could not be goes to `diagnostics` as an
/// error, module by module in the project's order and, within a module, in the order its graphs
/// meet the artifacts. The toolchain would download what is
/// missing and reports none of these problems here; krusty-toolchain refuses instead of printing
/// a graph it could not complete.
pub fn show(
    model: &Model,
    shown: impl Fn(&ModuleHeader) -> bool,
    include_tests: bool,
    diagnostics: &mut Diagnostics,
) -> Result<String, String> {
    let declarations = resolution::read_declarations(&model.modules, &model.configured)?;
    // The local Maven repository is read, before the cache, when any module lists it.
    let maven_local = declarations.iter().any(|module| module.maven_local);
    let store = match Store::for_project(maven_local) {
        Ok(store) => store,
        Err(OpenError::NoCacheRoot(message)) => return Err(message.to_string()),
        Err(OpenError::LocalRepository(problem)) => {
            diagnostics.push(problem.diagnostic());
            return Ok(String::new());
        }
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
