//! Carrying out a plan: each module's output directory cleared, krusty run with the module's
//! arguments, then its resources copied, modules running together as soon as the modules they
//! compile against are done (`schedule.rs`). krusty's output is captured per module and handed on
//! as each module finishes, so one module's output stays together. The first module that fails
//! stops new modules from starting. Both output directories are reconciled on every run, so a
//! module whose last source or resources directory is gone keeps no output from an earlier run.

use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command;

use super::plan::{Plan, Step};
use super::schedule::schedule;

/// Why a module failed.
#[derive(Debug)]
pub enum RunFailure {
    /// krusty failed on a module, exiting with `code`.
    Compiler { module: String, code: i32 },
    /// Something other than krusty failed: the message says what.
    Io(String),
}

/// What krusty printed for a module.
#[derive(Default)]
pub struct Printed {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// How many modules compile at once: as many as the host runs threads in parallel, as the
/// toolchain bounds its task graph by the available processors.
pub fn default_jobs() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

/// Run `plan` with the krusty binary `krusty`, up to `jobs` modules at once. `printed` sees what
/// krusty printed for each module it ran, in the order the modules finish. The failures are those
/// of the modules that failed, in the order they finished; once one fails no module starts, and
/// the modules running then are waited for.
pub fn run(
    plan: &Plan,
    krusty: &Path,
    jobs: NonZeroUsize,
    mut printed: impl FnMut(&Step, &Printed),
) -> Result<(), Vec<RunFailure>> {
    let waits: Vec<Vec<usize>> = plan.steps.iter().map(|step| step.waits.clone()).collect();
    let mut failures = Vec::new();
    schedule(
        &waits,
        jobs,
        |index| {
            let mut output = Printed::default();
            let result = run_step(&plan.steps[index], krusty, &mut output);
            (output, result)
        },
        |index, (output, result)| {
            printed(&plan.steps[index], &output);
            match result {
                Ok(()) => true,
                Err(failure) => {
                    failures.push(failure);
                    false
                }
            }
        },
    );
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

/// Carry out one module's step, keeping what krusty printed in `printed`.
fn run_step(step: &Step, krusty: &Path, printed: &mut Printed) -> Result<(), RunFailure> {
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
            let ran = Command::new(krusty)
                .args(arguments)
                .output()
                .map_err(|error| {
                    RunFailure::Io(format!(
                        "cannot run krusty ({}) for module `{}`: {error}",
                        krusty.display(),
                        step.module
                    ))
                })?;
            printed.stdout = ran.stdout;
            printed.stderr = ran.stderr;
            if !ran.status.success() {
                return Err(RunFailure::Compiler {
                    module: step.module.clone(),
                    // A krusty killed by a signal has no exit code of its own.
                    code: ran.status.code().unwrap_or(1),
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
            }),
        None => remove(&step.resources_output)
            .map_err(|error| output("remove", &step.resources_output, error)),
    }
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
