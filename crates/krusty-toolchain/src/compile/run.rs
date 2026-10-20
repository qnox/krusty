//! Carrying out a plan: each module's output directory cleared, krusty run with the module's
//! arguments, then its resources copied, unless its inputs and output are as its last successful
//! compilation left them (`avoidance.rs`), modules running together as soon as the modules they
//! compile against are done (`schedule.rs`). krusty's output is captured per module and handed on
//! whole, in plan order, so one module's output stays together and the order is the same however
//! the modules' compilations race. The first module that fails
//! stops new modules from starting. Both output directories are reconciled on every run, so a
//! module whose last source or resources directory is gone keeps no output from an earlier run.

use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command;

use super::avoidance;
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
/// krusty printed for each module it ran, in plan order: a module's output is handed on once it
/// and every module planned before it that ran have finished, so the order does not depend on
/// which finishes first. The failures are those of the modules that failed, in plan order; once one
/// fails no module begins (see `schedule.rs`), and the modules running then are waited for.
pub fn run(
    plan: &Plan,
    krusty: &Path,
    jobs: NonZeroUsize,
    mut printed: impl FnMut(&Step, &Printed),
) -> Result<(), Vec<RunFailure>> {
    let waits: Vec<Vec<usize>> = plan.steps.iter().map(|step| step.waits.clone()).collect();
    // Each step's output and result by plan index, once it has finished.
    let mut results: Vec<Option<(Printed, Result<(), RunFailure>)>> =
        plan.steps.iter().map(|_| None).collect();
    // The first step whose output has not been handed on.
    let mut next = 0;
    schedule(
        &waits,
        jobs,
        |index, gate| {
            // Nothing of a module is touched unless it begins before any module has failed.
            if !gate.begin() {
                return None;
            }
            let mut output = Printed::default();
            let result = run_step(&plan.steps[index], krusty, &mut output);
            if result.is_err() {
                gate.fail();
            }
            Some((output, result))
        },
        |index, done| {
            results[index] = Some(done);
            while let Some(Some((output, _))) = results.get(next) {
                printed(&plan.steps[next], output);
                next += 1;
            }
        },
    );
    // What is left waited for steps that never started.
    for (step, result) in plan.steps.iter().zip(&results).skip(next) {
        if let Some((output, _)) = result {
            printed(step, output);
        }
    }
    let failures: Vec<RunFailure> = results
        .into_iter()
        .flatten()
        .filter_map(|(_, result)| result.err())
        .collect();
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
            let inputs = avoidance::inputs(krusty, arguments);
            if inputs
                .as_deref()
                .is_some_and(|inputs| avoidance::up_to_date(&step.state, &step.output, inputs))
            {
                return copy_resources(step);
            }
            avoidance::forget(&step.state).map_err(|error| output("remove", &step.state, error))?;
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
            if let Some(inputs) = inputs {
                avoidance::record(&step.state, &step.output, &inputs)
                    .map_err(|error| output("record", &step.state, error))?;
            }
        }
        None => {
            avoidance::forget(&step.state).map_err(|error| output("remove", &step.state, error))?;
            remove(&step.output).map_err(|error| output("remove", &step.output, error))?;
        }
    }
    copy_resources(step)
}

/// Make the module's resources output exactly its resources directory's contents.
fn copy_resources(step: &Step) -> Result<(), RunFailure> {
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
        None => remove(&step.resources_output).map_err(|error| {
            RunFailure::Io(format!(
                "cannot remove {} for module `{}`: {error}",
                step.resources_output.display(),
                step.module
            ))
        }),
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
