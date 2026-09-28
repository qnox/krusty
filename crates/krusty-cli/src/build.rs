//! `krusty build`, the Kotlin Toolchain `kotlin build` command for JVM modules.
//!
//! The compiler command line is unchanged: `krusty src/main.kt` still invokes kotlinc-compatible
//! compilation. `build` is a subcommand only when it is the first argument, so a source file named
//! `build.kt` or a path `./build` stays a compiler input.

use std::path::PathBuf;

use krusty_build::kotlin_toolchain::{execute, BuildCommand};

pub const HELP: &str = "\
usage: krusty build [options]

Compile a Kotlin Toolchain JVM project in the working directory (module.yaml or project.yaml).
This is the krusty stand-in for `kotlin build`. Gradle and .iml projects are recognized and left
to their project-model extensions.

Options:
  -m, --module <module>   build this module (repeatable). Dependencies are included.
  -p, --platform <platform>
                          platform to build. Only jvm is supported.
  -v, --variant <variant> debug or release. A JVM module is compiled once either way.
  -h, --help              print this help and exit
";

pub fn is_build_command(args: &[String]) -> bool {
    args.first().is_some_and(|argument| argument == "build")
}

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(Parsed::Help) => {
            println!("{HELP}");
            return 0;
        }
        Ok(Parsed::Build(mut command)) => {
            match std::env::current_exe() {
                Ok(compiler) => command.compiler = compiler,
                Err(error) => {
                    eprintln!("krusty: cannot locate the krusty binary: {error}");
                    return 1;
                }
            }
            match std::env::current_dir() {
                Ok(directory) => command.directory = directory,
                Err(error) => {
                    eprintln!("krusty: cannot determine the working directory: {error}");
                    return 1;
                }
            }
            command
        }
        Err(message) => {
            eprintln!("krusty: {message}");
            return 1;
        }
    };
    match execute(&parsed) {
        Ok(report) => {
            print!("{}", report.render());
            if report.is_success() {
                println!("Build successful");
                0
            } else {
                1
            }
        }
        Err(message) => {
            eprintln!("krusty: {message}");
            1
        }
    }
}

#[derive(Debug)]
enum Parsed {
    Help,
    Build(BuildCommand),
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut modules = Vec::new();
    let mut platforms = Vec::new();
    let mut variants = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "-h" || argument == "--help" {
            return Ok(Parsed::Help);
        }
        if let Some(value) = inline_value(argument, &["--module", "-m"]) {
            push_value("module", value, &mut modules)?;
            index += 1;
            continue;
        }
        if argument == "--module" || argument == "-m" {
            index += 1;
            push_value("module", required(args, index, "--module")?, &mut modules)?;
            index += 1;
            continue;
        }
        if let Some(value) = inline_value(argument, &["--platform", "-p"]) {
            push_value("platform", value, &mut platforms)?;
            index += 1;
            continue;
        }
        if argument == "--platform" || argument == "-p" {
            index += 1;
            push_value(
                "platform",
                required(args, index, "--platform")?,
                &mut platforms,
            )?;
            index += 1;
            continue;
        }
        if let Some(value) = inline_value(argument, &["--variant", "-v"]) {
            push_value("variant", value, &mut variants)?;
            index += 1;
            continue;
        }
        if argument == "--variant" || argument == "-v" {
            index += 1;
            push_value(
                "variant",
                required(args, index, "--variant")?,
                &mut variants,
            )?;
            index += 1;
            continue;
        }
        return Err(format!("unrecognized option '{argument}'"));
    }
    Ok(Parsed::Build(BuildCommand {
        directory: PathBuf::new(),
        modules: dedup(modules),
        platforms: dedup(platforms),
        variants: dedup(variants),
        compiler: PathBuf::new(),
    }))
}

fn inline_value<'a>(argument: &'a str, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        argument
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

fn required<'a>(args: &'a [String], index: usize, name: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} requires a value"))
}

fn push_value(kind: &str, value: &str, out: &mut Vec<String>) -> Result<(), String> {
    if value.is_empty() || value.starts_with('-') {
        return Err(format!("{kind} name expected, found '{value}'"));
    }
    out.push(value.to_string());
    Ok(())
}

fn dedup(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn build_is_a_subcommand_only_as_the_first_argument() {
        assert!(is_build_command(&args(&["build"])));
        assert!(is_build_command(&args(&["build", "--module", "app"])));
        assert!(!is_build_command(&args(&["build.kt"])));
        assert!(!is_build_command(&args(&["./build"])));
        assert!(!is_build_command(&args(&["-d", "out", "build"])));
    }

    #[test]
    fn module_platform_and_variant_options_match_the_toolchain_spelling() {
        let Parsed::Build(command) = parse(&args(&[
            "--module",
            "app",
            "-m",
            "lib",
            "--module=app",
            "--platform",
            "jvm",
            "-v",
            "release",
        ]))
        .expect("parse") else {
            panic!("expected a build command");
        };
        assert_eq!(command.modules, vec!["app".to_string(), "lib".to_string()]);
        assert_eq!(command.platforms, vec!["jvm".to_string()]);
        assert_eq!(command.variants, vec!["release".to_string()]);
    }

    #[test]
    fn help_missing_values_and_unknown_options_are_reported_in_full() {
        assert!(matches!(parse(&args(&["-h"])).unwrap(), Parsed::Help));
        assert_eq!(
            parse(&args(&["--module"])).unwrap_err(),
            "--module requires a value"
        );
        assert_eq!(
            parse(&args(&["--platform", "--module"])).unwrap_err(),
            "platform name expected, found '--module'"
        );
        assert_eq!(
            parse(&args(&["--clean"])).unwrap_err(),
            "unrecognized option '--clean'"
        );
    }
}
