//! Carrying out a plan: each module in order, its output directory cleared, krusty run with the
//! module's arguments and its output streamed as it comes, then its resources copied; the first
//! module krusty fails on stops the task. Both output directories are reconciled on every run, so
//! a module whose last source or resources directory is gone keeps no output from an earlier run.

use std::io;
use std::path::Path;
use std::process::Command;

use super::plan::Plan;

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
        let output = |action: &str, directory: &Path, error: io::Error| {
            RunFailure::Io(format!(
                "cannot {action} {} for module `{}`: {error}",
                directory.display(),
                step.module
            ))
        };
        match &step.arguments {
            Some(arguments) => {
                clear(&step.output).map_err(|error| output("clear", &step.output, error))?;
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
            None => remove(&step.output).map_err(|error| output("remove", &step.output, error))?,
        }
        match &step.resources {
            Some(resources) => clear(&step.resources_output)
                .and_then(|()| copy_tree(resources, &step.resources_output))
                .map_err(|error| {
                    RunFailure::Io(format!(
                        "cannot copy the resources of module `{}` from {} to {}: {error}",
                        step.module,
                        resources.display(),
                        step.resources_output.display()
                    ))
                })?,
            None => remove(&step.resources_output)
                .map_err(|error| output("remove", &step.resources_output, error))?,
        }
    }
    Ok(())
}

/// Make `directory` an empty directory.
fn clear(directory: &Path) -> io::Result<()> {
    remove(directory)?;
    std::fs::create_dir_all(directory)
}

/// Remove `directory` and everything in it, if it exists.
fn remove(directory: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
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
