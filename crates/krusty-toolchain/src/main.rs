//! `krusty-toolchain`: builds a Kotlin Toolchain project (`project.yaml`, `module.yaml`) with krusty.
//!
//! Commands read the project as JetBrains' `kotlin` command reads it and report the same problems;
//! what krusty-toolchain does not implement is refused by name rather than approximated.

use std::path::PathBuf;
use std::process::ExitCode;

use krusty_toolchain::diagnostic::{Diagnostics, Severity};
use krusty_toolchain::model::{self, Start};
use krusty_toolchain::show;

const USAGE: &str =
    "usage: krusty-toolchain [--project-dir=<path>] show modules [--format=plain|table]";

#[derive(Clone, Copy)]
enum Format {
    Plain,
    Table,
}

struct Command {
    project_dir: Option<PathBuf>,
    format: Format,
}

fn parse(arguments: &[String]) -> Result<Command, String> {
    let mut project_dir = None;
    let mut format = Format::Table;
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
            format,
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
        match diagnostic.severity {
            Severity::Error => eprintln!("{diagnostic}"),
            Severity::Warning | Severity::WeakWarning => println!("{diagnostic}"),
        }
    }
    let Some(model) = model? else {
        if !diagnostics.has_errors() {
            return Err("no Kotlin project found in the current directory or above: no project.yaml or module.yaml".to_string());
        }
        return Ok(false);
    };
    match command.format {
        Format::Table => print!("{}", show::modules_table(&model.modules)),
        Format::Plain => {
            for name in show::module_names(&model.modules) {
                println!("{name}");
            }
        }
    }
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
