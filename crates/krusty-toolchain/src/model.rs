//! The project model a command works on: the project and its modules, read in the toolchain's
//! order, stopping where it stops.

use std::path::Path;

use crate::diagnostic::Diagnostics;
use crate::module::{self, ModuleHeader};
use crate::project::{self, Project};

/// Where a command looks for its project.
pub enum Start<'a> {
    /// The nearest project at or above this directory.
    Discover(&'a Path),
    /// The project rooted exactly here.
    Root(&'a Path),
}

pub struct Model {
    pub project: Project,
    pub modules: Vec<ModuleHeader>,
}

/// Read the project `start` names. Problems go to `diagnostics`; `Ok(None)` when there is no
/// project or when an error was reported. The project file is read first, and its errors stop
/// before any module file is read; module files' errors stop before module names are compared.
pub fn read(start: Start<'_>, diagnostics: &mut Diagnostics) -> Result<Option<Model>, String> {
    let project = match start {
        Start::Discover(directory) => project::load(directory, diagnostics)?,
        Start::Root(root) => project::load_root(root, diagnostics)?,
    };
    let Some(project) = project else {
        return Ok(None);
    };
    if diagnostics.has_errors() {
        return Ok(None);
    }
    let Some(modules) = module::read_headers(&project.modules, diagnostics) else {
        return Ok(None);
    };
    project::check_unique_names(&project, diagnostics);
    if diagnostics.has_errors() {
        return Ok(None);
    }
    Ok(Some(Model { project, modules }))
}
