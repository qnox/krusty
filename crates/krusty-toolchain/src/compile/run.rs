//! Carrying out a plan: each module in order, its output directory cleared, krusty run with the
//! module's arguments and its output streamed as it comes, then its resources copied; the first
//! module krusty fails on stops the task.

use std::io;
use std::path::Path;
use std::process::Command;

use super::plan::{Plan, Resources};

/// Why a plan stopped.
#[derive(Debug)]
pub enum RunFailure {
    /// krusty failed on a module, exiting with `code`.
    Compiler { module: String, code: i32 },
    /// Something other than krusty failed: the message says what.
    Io(String),
}

/// Run `plan` with the krusty binary `krusty`.
pub fn run(plan: &Plan, krusty: &Path) -> Result<(), RunFailure> {
    for step in &plan.steps {
        if let Some(arguments) = &step.arguments {
            clear(&step.output).map_err(|error| {
                RunFailure::Io(format!(
                    "cannot clear {} for module `{}`: {error}",
                    step.output.display(),
                    step.module
                ))
            })?;
            let status = Command::new(krusty)
                .args(arguments)
                .status()
                .map_err(|error| {
                    RunFailure::Io(format!(
                        "cannot run krusty ({}) for module `{}`: {error}",
                        krusty.display(),
                        step.module
                    ))
                })?;
            if !status.success() {
                return Err(RunFailure::Compiler {
                    module: step.module.clone(),
                    // A krusty killed by a signal has no exit code of its own.
                    code: status.code().unwrap_or(1),
                });
            }
        }
        if let Some(resources) = &step.resources {
            copy_resources(resources).map_err(|error| {
                RunFailure::Io(format!(
                    "cannot copy the resources of module `{}` from {} to {}: {error}",
                    step.module,
                    resources.from.display(),
                    resources.to.display()
                ))
            })?;
        }
    }
    Ok(())
}

/// Make `directory` an empty directory.
fn clear(directory: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(directory)
}

/// Replace the copied resources with the resources directory's contents.
fn copy_resources(resources: &Resources) -> io::Result<()> {
    clear(&resources.to)?;
    copy_tree(&resources.from, &resources.to)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
