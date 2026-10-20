//! `krusty-toolchain`: builds a Kotlin Toolchain project (`project.yaml`, `module.yaml`) with krusty.
//!
//! Commands read the project as JetBrains' `kotlin` command reads it and report the same problems;
//! what krusty-toolchain does not implement is refused by name rather than approximated.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use krusty_toolchain::compile;
use krusty_toolchain::dependencies;
use krusty_toolchain::diagnostic::{Diagnostics, Severity};
use krusty_toolchain::model::{self, Model, Problems, Start, Stopped};
use krusty_toolchain::report;
use krusty_toolchain::show;

const USAGE: &str =
    "usage: krusty-toolchain [--project-dir=<path>] show modules [--format=plain|table]
       krusty-toolchain [--project-dir=<path>] show settings [-m <module>]... [--all-modules]
       krusty-toolchain [--project-dir=<path>] show dependencies [-m <module>]... [--all-modules] [--include-tests]
       krusty-toolchain [--project-dir=<path>] task :<module>:compileJvm...";

#[derive(Clone, Copy)]
enum Format {
    Plain,
    Table,
}

/// The modules a command is about: those named, or every module (`all`).
struct Selection {
    names: Vec<String>,
    all: bool,
}

enum Show {
    Modules(Format),
    Settings(Selection),
    Dependencies {
        modules: Selection,
        include_tests: bool,
    },
}

enum Action {
    Show(Show),
    /// Run the tasks named `:<module>:<task>`, together.
    Task(Vec<String>),
}

struct Command {
    project_dir: Option<PathBuf>,
    action: Action,
}

fn parse(arguments: &[String]) -> Result<Command, String> {
    let mut project_dir = None;
    let mut format = Format::Table;
    let mut modules = Vec::new();
    let mut all_modules = false;
    let mut include_tests = false;
    let mut words = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let (name, inline) = match argument.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
            _ => (argument.as_str(), None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            arguments
                .get(index)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match name {
            "--project-dir" => project_dir = Some(PathBuf::from(value()?)),
            "--format" => {
                format = match value()?.as_str() {
                    "plain" => Format::Plain,
                    "table" => Format::Table,
                    other => {
                        return Err(format!("unknown format `{other}`: use `plain` or `table`"))
                    }
                }
            }
            "-m" | "--module" => modules.push(value()?),
            "-a" | "--all-modules" => all_modules = true,
            "--include-tests" => include_tests = true,
            "--exclude-tests" => include_tests = false,
            option if option.starts_with('-') => {
                return Err(format!(
                    "krusty-toolchain does not implement the option `{option}`"
                ))
            }
            word => words.push(word.to_string()),
        }
        index += 1;
    }
    match words
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["show", "modules"] => Ok(Command {
            project_dir,
            action: Action::Show(Show::Modules(format)),
        }),
        ["show", "settings"] => Ok(Command {
            project_dir,
            action: Action::Show(Show::Settings(Selection {
                names: modules,
                all: all_modules,
            })),
        }),
        ["show", "dependencies"] => Ok(Command {
            project_dir,
            action: Action::Show(Show::Dependencies {
                modules: Selection {
                    names: modules,
                    all: all_modules,
                },
                include_tests,
            }),
        }),
        ["task", tasks @ ..] if !tasks.is_empty() => Ok(Command {
            project_dir,
            action: Action::Task(tasks.iter().map(|task| task.to_string()).collect()),
        }),
        ["task"] => Err("`task` needs the task to run, such as `:<module>:compileJvm`".to_string()),
        [] => Err("no command given".to_string()),
        other => Err(format!(
            "krusty-toolchain does not implement the command `{}`",
            other.join(" ")
        )),
    }
}

/// Why a command failed, as the toolchain says it.
enum Failure {
    /// The problems reported say why; the toolchain prints nothing more.
    Reported,
    /// The project file has errors, reported above.
    ProjectFile,
    /// A module file or a module's configuration has errors, reported above.
    Model,
    /// A mistake by the user that is not a problem in a file, such as an unknown module name.
    User(String),
    /// Modules failed, krusty on some of them: each failure, in plan order.
    Tasks(Vec<compile::RunFailure>),
}

fn run(command: Command) -> Result<(), Failure> {
    let current = std::env::current_dir()
        .map_err(|error| Failure::User(format!("cannot read the current directory: {error}")))?;
    let start = match &command.project_dir {
        Some(root) => Start::Root(root),
        None => Start::Discover(&current),
    };
    let mut problems = Problems::default();
    let read = model::read(start, &mut problems);
    // The project file is read before the project's root is known, so its files are named as they
    // are; the modules' are named relative to the root.
    report(&problems.project_file, None);
    report(&problems.modules, problems.root.as_deref());
    let model = read.map_err(|stopped| match stopped {
        Stopped::NoProject => Failure::User(
            "no Kotlin project found in the current directory or above: no project.yaml or module.yaml".to_string(),
        ),
        Stopped::ProjectFile => Failure::ProjectFile,
        Stopped::Modules => Failure::Model,
        Stopped::NotUnique(message) | Stopped::Failed(message) => Failure::User(message),
    })?;
    let root = model.project.root.clone();
    let show = match command.action {
        Action::Show(show) => show,
        Action::Task(tasks) => return run_tasks(&model, &tasks),
    };
    match show {
        Show::Modules(Format::Table) => print!("{}", show::modules_table(&model.modules)),
        Show::Modules(Format::Plain) => {
            for name in show::module_names(&model.modules) {
                println!("{name}");
            }
        }
        Show::Settings(selection) => {
            let shown = selected(&model, &selection)?;
            print!(
                "{}",
                show::modules_settings(&model.modules, &model.configured, |module| {
                    shown.contains(&module.name.as_str())
                })
            );
        }
        Show::Dependencies {
            modules,
            include_tests,
        } => {
            let shown = selected(&model, &modules)?;
            let mut diagnostics = Diagnostics::default();
            let output = dependencies::show(
                &model,
                |module| shown.contains(&module.name.as_str()),
                include_tests,
                &mut diagnostics,
            )
            .map_err(Failure::User)?;
            report(&diagnostics, Some(&root));
            if diagnostics.has_errors() {
                return Err(Failure::Reported);
            }
            print!("{output}");
        }
    }
    Ok(())
}

/// Run the tasks `tasks` (each `:<module>:compileJvm`, the only task krusty-toolchain runs) of
/// `model` together: the modules they need, each compiled once.
fn run_tasks(model: &Model, tasks: &[String]) -> Result<(), Failure> {
    let mut named = Vec::new();
    let mut unknown = None;
    for task in tasks {
        let found = task
            .strip_prefix(':')
            .and_then(|task| task.split_once(':'))
            .filter(|(module, _)| model.modules.iter().any(|header| header.name == *module));
        match found {
            Some((module, name)) => named.push((task, module, name)),
            // The toolchain names the last task it does not find.
            None => unknown = Some(task),
        }
    }
    if let Some(task) = unknown {
        return Err(Failure::User(format!(
            "Task '{task}' was not found in the project"
        )));
    }
    if let Some((task, _, _)) = named.iter().find(|(_, _, name)| *name != compile::TASK) {
        return Err(Failure::User(format!(
            "krusty-toolchain does not implement the task '{task}': it runs only `:<module>:{}`",
            compile::TASK
        )));
    }
    let modules: Vec<&str> = named.iter().map(|(_, module, _)| *module).collect();
    let environment = compile::Environment {
        java_home: std::env::var_os("JAVA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    };
    let mut diagnostics = Diagnostics::default();
    let plan =
        compile::plan(model, &modules, &environment, &mut diagnostics).map_err(Failure::User)?;
    report(&diagnostics, Some(&model.project.root));
    let Some(plan) = plan else {
        return Err(Failure::Reported);
    };
    let krusty = std::env::var_os("KRUSTY_BIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            Failure::User(
                "KRUSTY_BIN is not set: krusty-toolchain compiles with the krusty binary it names, and does not search PATH for one".to_string(),
            )
        })?;
    compile::run(&plan, &krusty, compile::default_jobs(), |_, printed| {
        // Each module's output at once, so modules compiling together do not interleave.
        let _ = std::io::stdout().lock().write_all(&printed.stdout);
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().lock().write_all(&printed.stderr);
    })
    .map_err(Failure::Tasks)
}

/// Print problems where the toolchain prints them: errors on stderr, warnings on stdout before the
/// command's result. Files are named relative to `root` when it is given.
fn report(diagnostics: &Diagnostics, root: Option<&Path>) {
    for diagnostic in diagnostics.iter() {
        let rendered = report::render(diagnostic, root);
        match diagnostic.severity {
            Severity::Error => eprintln!("{rendered}"),
            Severity::Warning | Severity::WeakWarning => println!("{rendered}"),
        }
    }
}

/// The modules `selection` names (every module when `all`, or when there is only one), in the
/// project's order.
fn selected<'m>(model: &'m Model, selection: &Selection) -> Result<Vec<&'m str>, Failure> {
    let Selection { names, all } = selection;
    if names.is_empty() && !all && model.modules.len() > 1 {
        return Err(Failure::User("Please specify the module(s) to inspect with --module, or use --all-modules to inspect all modules".to_string()));
    }
    let unknown: Vec<&str> = names
        .iter()
        .filter(|name| !model.modules.iter().any(|module| module.name == **name))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let available = show::module_names(&model.modules);
        return Err(Failure::User(format!(
            "Couldn't find module(s) named: {}\nAvailable modules: - {}",
            unknown.join(", "),
            available.join("\n- ")
        )));
    }
    Ok(model
        .modules
        .iter()
        .filter(|module| *all || names.is_empty() || names.contains(&module.name))
        .map(|module| module.name.as_str())
        .collect())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&arguments) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("krusty-toolchain: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Err(failure) = run(command) else {
        return ExitCode::SUCCESS;
    };
    // The toolchain sets the line saying why it stopped apart from what it printed before.
    match failure {
        Failure::Reported => {}
        Failure::ProjectFile => eprintln!(
            "\nERROR: Aborting because there were errors in the Kotlin project file, please see above."
        ),
        Failure::Model => {
            eprintln!("\nERROR: failed to read Kotlin project model, refer to the errors above")
        }
        Failure::User(message) => eprintln!("\nERROR: {message}"),
        Failure::Tasks(failures) => {
            // The status is the first failure's: krusty's for a compilation.
            let mut status = None;
            for failure in failures {
                match failure {
                    compile::RunFailure::Compiler { module, code } => {
                        eprintln!(
                            "\nERROR: Task ':{module}:{}' failed: krusty exited with status {code}",
                            compile::TASK
                        );
                        status.get_or_insert(code);
                    }
                    compile::RunFailure::Io(message) => {
                        eprintln!("\nERROR: {message}");
                        status.get_or_insert(1);
                    }
                }
            }
            let code = status.unwrap_or(1);
            return ExitCode::from(u8::try_from(code).ok().filter(|&code| code != 0).unwrap_or(1));
        }
    }
    ExitCode::FAILURE
}
