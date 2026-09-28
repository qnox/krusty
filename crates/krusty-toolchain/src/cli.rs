//! Argument parsing for the `krusty-toolchain` executable.
//!
//! Only `build` is implemented. Other toolchain commands fail closed so they are not mistaken for
//! compiler inputs or silently ignored.

use std::path::{Path, PathBuf};

use krusty_build::kotlin_toolchain::BuildCommand;

pub const ROOT_HELP: &str = "\
usage: krusty-toolchain <command> [options]

krusty stand-in for the Kotlin Toolchain CLI (`kotlin build`). The compiler is the separate
`krusty` executable (`KRUSTY_COMPILER`, the same directory as this program, or `PATH`).

Commands:
  build                   compile a JVM project

A module.yaml or project.yaml project is read directly. Gradle and Maven projects are compiled
by running that tool; their build scripts and POMs are not parsed. A JetBrains .iml project is
recognized and left to its project-model extension.
";

pub const BUILD_HELP: &str = "\
usage: krusty-toolchain build [options]

Compile a JVM project in the working directory. A module.yaml or project.yaml project is read
directly. A Gradle or Maven project is compiled by running that tool; its descriptors are not
parsed. A JetBrains .iml project is recognized and left to its project-model extension.
Same options as the toolchain `kotlin build` command.

Options:
  -m, --module <module>   build this module (repeatable). Dependencies are included.
  -p, --platform <platform>
                          platform to build. Only jvm is supported.
  -v, --variant <variant> debug or release. A JVM module is compiled once either way.
  -h, --help              print this help and exit
";

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Help(&'static str),
    Build(BuildCommand),
}

/// Where to find the `krusty` compiler for a `krusty-toolchain` invocation.
#[derive(Clone, Debug)]
pub struct CompilerSearch {
    /// `KRUSTY_COMPILER`, when the variable is set. An explicit path that is not a file is an error.
    pub override_path: Option<PathBuf>,
    /// This `krusty-toolchain` executable. A sibling `krusty` is preferred over `PATH`.
    pub executable: PathBuf,
    /// Directories from `PATH`, in order.
    pub path_dirs: Vec<PathBuf>,
}

pub fn parse(args: &[String]) -> Result<Parsed, String> {
    match args.first().map(String::as_str) {
        None => Err(format!("a command is required\n\n{ROOT_HELP}")),
        Some("-h" | "--help") => Ok(Parsed::Help(ROOT_HELP)),
        Some("build") => parse_build(&args[1..]),
        Some(command) => Err(format!("unrecognized command '{command}'\n\n{ROOT_HELP}")),
    }
}

pub fn locate_compiler(search: &CompilerSearch) -> Result<PathBuf, String> {
    if let Some(path) = &search.override_path {
        if path.is_file() {
            return Ok(path.clone());
        }
        return Err(format!("KRUSTY_COMPILER is not a file: {}", path.display()));
    }
    if let Some(sibling) = sibling_compiler(&search.executable) {
        return Ok(sibling);
    }
    for directory in &search.path_dirs {
        let candidate = directory.join(compiler_file_name());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(
        "cannot find the krusty compiler. Set KRUSTY_COMPILER, or place krusty next to this executable."
            .to_string(),
    )
}

fn sibling_compiler(executable: &Path) -> Option<PathBuf> {
    let candidate = executable.parent()?.join(compiler_file_name());
    candidate.is_file().then_some(candidate)
}

fn compiler_file_name() -> &'static str {
    if cfg!(windows) {
        "krusty.exe"
    } else {
        "krusty"
    }
}

fn parse_build(args: &[String]) -> Result<Parsed, String> {
    let mut modules = Vec::new();
    let mut platforms = Vec::new();
    let mut variants = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "-h" || argument == "--help" {
            return Ok(Parsed::Help(BUILD_HELP));
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

    fn command(parsed: Parsed) -> BuildCommand {
        match parsed {
            Parsed::Build(command) => command,
            Parsed::Help(_) => panic!("expected a build command"),
        }
    }

    #[test]
    fn build_is_a_command_of_the_toolchain_executable() {
        let parsed = parse(&args(&[
            "build",
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
        .expect("parse");
        let command = command(parsed);
        assert_eq!(command.modules, vec!["app".to_string(), "lib".to_string()]);
        assert_eq!(command.platforms, vec!["jvm".to_string()]);
        assert_eq!(command.variants, vec!["release".to_string()]);
    }

    #[test]
    fn help_missing_values_and_unknown_commands_are_reported_in_full() {
        assert_eq!(parse(&args(&["--help"])).unwrap(), Parsed::Help(ROOT_HELP));
        assert_eq!(
            parse(&args(&["build", "-h"])).unwrap(),
            Parsed::Help(BUILD_HELP)
        );
        assert_eq!(
            parse(&args(&["build", "--help"])).unwrap(),
            Parsed::Help(BUILD_HELP)
        );
        assert_eq!(
            parse(&args(&[])).unwrap_err(),
            format!("a command is required\n\n{ROOT_HELP}")
        );
        assert_eq!(
            parse(&args(&["test"])).unwrap_err(),
            format!("unrecognized command 'test'\n\n{ROOT_HELP}")
        );
        assert_eq!(
            parse(&args(&["build", "--module"])).unwrap_err(),
            "--module requires a value"
        );
        assert_eq!(
            parse(&args(&["build", "--platform", "--module"])).unwrap_err(),
            "platform name expected, found '--module'"
        );
        assert_eq!(
            parse(&args(&["build", "--clean"])).unwrap_err(),
            "unrecognized option '--clean'"
        );
        assert_eq!(parse(&args(&["-h"])).unwrap(), Parsed::Help(ROOT_HELP));
        assert_eq!(
            parse(&args(&["build", "--variant"])).unwrap_err(),
            "--variant requires a value"
        );
        assert_eq!(
            parse(&args(&["build", "-p"])).unwrap_err(),
            "--platform requires a value"
        );
        assert_eq!(
            parse(&args(&["build", "-m", "-v"])).unwrap_err(),
            "module name expected, found '-v'"
        );
        assert_eq!(
            parse(&args(&["build", "--module="])).unwrap_err(),
            "module name expected, found ''"
        );
        assert_eq!(
            parse(&args(&["build", "--platform="])).unwrap_err(),
            "platform name expected, found ''"
        );
        assert_eq!(
            parse(&args(&["build", "--variant="])).unwrap_err(),
            "variant name expected, found ''"
        );
        assert_eq!(
            parse(&args(&["build", "--module", ""])).unwrap_err(),
            "--module requires a value"
        );
    }

    #[test]
    fn inline_flags_and_repeated_modules_collapse_to_one_command() {
        let command = command(
            parse(&args(&[
                "build",
                "--platform=jvm",
                "-p=jvm",
                "--variant=debug",
                "-m",
                "app",
                "--module",
                "app",
                "-v",
                "debug",
            ]))
            .expect("parse"),
        );
        assert_eq!(command.modules, vec!["app".to_string()]);
        assert_eq!(command.platforms, vec!["jvm".to_string()]);
        assert_eq!(command.variants, vec!["debug".to_string()]);
    }

    #[test]
    fn compiler_search_prefers_an_explicit_path_then_a_sibling_then_path() {
        let root = std::env::temp_dir().join(format!(
            "krusty-toolchain-compiler-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let elsewhere = root.join("elsewhere");
        let path_dir = root.join("path");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        std::fs::create_dir_all(&path_dir).expect("path");
        let sibling = bin.join(compiler_file_name());
        let explicit = elsewhere.join("krusty-explicit");
        let on_path = path_dir.join(compiler_file_name());
        std::fs::write(&sibling, b"sibling").expect("sibling");
        std::fs::write(&explicit, b"explicit").expect("explicit");
        std::fs::write(&on_path, b"path").expect("path");

        let executable = bin.join("krusty-toolchain");
        let found = locate_compiler(&CompilerSearch {
            override_path: Some(explicit.clone()),
            executable: executable.clone(),
            path_dirs: vec![path_dir.clone()],
        })
        .expect("explicit compiler");
        assert_eq!(found, explicit);

        let missing = elsewhere.join("missing-krusty");
        let error = locate_compiler(&CompilerSearch {
            override_path: Some(missing.clone()),
            executable: executable.clone(),
            path_dirs: vec![path_dir.clone()],
        })
        .expect_err("missing override");
        assert_eq!(
            error,
            format!("KRUSTY_COMPILER is not a file: {}", missing.display())
        );

        let found = locate_compiler(&CompilerSearch {
            override_path: None,
            executable: executable.clone(),
            path_dirs: vec![path_dir.clone()],
        })
        .expect("sibling compiler");
        assert_eq!(found, sibling);

        std::fs::remove_file(&sibling).expect("remove sibling");
        let found = locate_compiler(&CompilerSearch {
            override_path: None,
            executable: executable.clone(),
            path_dirs: vec![root.join("empty"), path_dir],
        })
        .expect("path compiler");
        assert_eq!(found, on_path);

        let error = locate_compiler(&CompilerSearch {
            override_path: None,
            executable,
            path_dirs: vec![root.join("empty")],
        })
        .expect_err("no compiler");
        assert_eq!(
            error,
            "cannot find the krusty compiler. Set KRUSTY_COMPILER, or place krusty next to this executable."
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
