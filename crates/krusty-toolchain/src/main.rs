//! `krusty-toolchain`: builds a Kotlin Toolchain project (`project.yaml`, `module.yaml`) with krusty.
//!
//! Commands read the project as JetBrains' `kotlin` command reads it and report the same problems;
//! what krusty-toolchain does not implement is refused by name rather than approximated.

use std::path::PathBuf;
use std::process::ExitCode;

use krusty_toolchain::configuration;
use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::model::{self, Model, Start};
use krusty_toolchain::show;

const USAGE: &str =
    "usage: krusty-toolchain [--project-dir=<path>] show modules [--format=plain|table]
       krusty-toolchain [--project-dir=<path>] show settings [-m <module>]... [--all-modules]";

#[derive(Clone, Copy)]
enum Format {
    Plain,
    Table,
}

enum Show {
    Modules(Format),
    /// The named modules', or every module's (`all`).
    Settings {
        names: Vec<String>,
        all: bool,
    },
}

struct Command {
    project_dir: Option<PathBuf>,
    show: Show,
}

fn parse(arguments: &[String]) -> Result<Command, String> {
    let mut project_dir = None;
    let mut format = Format::Table;
    let mut modules = Vec::new();
    let mut all_modules = false;
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
            show: Show::Modules(format),
        }),
        ["show", "settings"] => Ok(Command {
            project_dir,
            show: Show::Settings {
                names: modules,
                all: all_modules,
            },
        }),
        [] => Err("no command given".to_string()),
        other => Err(format!(
            "krusty-toolchain does not implement the command `{}`",
            other.join(" ")
        )),
    }
}

fn run(command: Command) -> Result<bool, String> {
    let current = std::env::current_dir()
        .map_err(|error| format!("cannot read the current directory: {error}"))?;
    let start = match &command.project_dir {
        Some(root) => Start::Root(root),
        None => Start::Discover(&current),
    };
    let mut diagnostics = Diagnostics::default();
    let model = model::read(start, &mut diagnostics);
    for diagnostic in diagnostics.iter() {
        eprintln!("{diagnostic}");
    }
    let Some(model) = model? else {
        if !diagnostics.has_errors() {
            return Err("no Kotlin project found in the current directory or above: no project.yaml or module.yaml".to_string());
        }
        return Ok(false);
    };
    match command.show {
        Show::Modules(Format::Table) => print!("{}", show::modules_table(&model.modules)),
        Show::Modules(Format::Plain) => {
            for name in show::module_names(&model.modules) {
                println!("{name}");
            }
        }
        Show::Settings { names, all } => return show_settings(&model, &names, all),
    }
    Ok(true)
}

/// Print the settings of the modules `names` (every module when `all`, or when there is only one),
/// in the project's order.
fn show_settings(model: &Model, names: &[String], all: bool) -> Result<bool, String> {
    if names.is_empty() && !all && model.modules.len() > 1 {
        return Err("Please specify the module(s) to inspect with -m, or use --all-modules to inspect all modules".to_string());
    }
    let unknown: Vec<&String> = names
        .iter()
        .filter(|name| !model.modules.iter().any(|module| module.name == **name))
        .collect();
    if !unknown.is_empty() {
        let available = show::module_names(&model.modules);
        return Err(format!(
            "Couldn't find module(s) named: {}\nAvailable modules: - {}",
            unknown
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            available.join("\n- ")
        ));
    }
    let mut diagnostics = Diagnostics::default();
    let configured =
        configuration::configure(&model.project.root, &model.modules, &mut diagnostics);
    let output = show::modules_settings(&model.modules, &configured, |module| {
        all || names.is_empty() || names.contains(&module.name)
    });
    for diagnostic in diagnostics.iter() {
        eprintln!("{diagnostic}");
    }
    if diagnostics.has_errors() {
        return Ok(false);
    }
    print!("{output}");
    Ok(true)
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
    match run(command) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("ERROR: {message}");
            ExitCode::FAILURE
        }
    }
}
