//! The project model a command works on: the project, its modules and their configuration, read
//! in the toolchain's order, stopping where it stops.

use std::path::{Path, PathBuf};

use crate::configuration::{self, Configuration};
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
    /// Each module's configuration, in the modules' order.
    pub configured: Vec<Configuration>,
}

/// The problems found reading a model, by the step that found them: the toolchain reports the
/// project file's before the project root is known, naming files as they are, and the modules'
/// relative to the root.
#[derive(Default)]
pub struct Problems {
    pub project_file: Diagnostics,
    pub modules: Diagnostics,
    /// The project's root, once the project file is read.
    pub root: Option<PathBuf>,
}

/// Why no model was read.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Neither a project file nor a module file at or above the start.
    NoProject,
    /// The project file has errors, so no module file was read.
    ProjectFile,
    /// A module file or a module's configuration has errors.
    Modules,
    /// Two modules share a name; the message names them.
    NotUnique(String),
    /// Something other than the project's files went wrong: the message says what.
    Failed(String),
}

/// Read the project `start` names. The project file is read first, and its errors stop before any
/// module file is read; module files' and configurations' errors stop before module names are
/// compared.
pub fn read(start: Start<'_>, problems: &mut Problems) -> Result<Model, Stopped> {
    let project = match start {
        Start::Discover(directory) => project::load(directory, &mut problems.project_file),
        Start::Root(root) => project::load_root(root, &mut problems.project_file),
    }
    .map_err(Stopped::Failed)?;
    let Some(project) = project else {
        return Err(if problems.project_file.has_errors() {
            Stopped::ProjectFile
        } else {
            Stopped::NoProject
        });
    };
    if problems.project_file.has_errors() {
        return Err(Stopped::ProjectFile);
    }
    problems.root = Some(project.root.clone());
    let modules =
        module::read_headers(&project.modules, &mut problems.modules).ok_or(Stopped::Modules)?;
    let configured = configuration::configure(&project.root, &modules, &mut problems.modules);
    if problems.modules.has_errors() {
        return Err(Stopped::Modules);
    }
    project::check_unique_names(&project).map_err(Stopped::NotUnique)?;
    Ok(Model {
        project,
        modules,
        configured,
    })
}
