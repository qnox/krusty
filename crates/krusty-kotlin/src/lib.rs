//! Process front end for the Kotlin Toolchain CLI.
//!
//! The `kotlin` executable is separate from the `krusty` compiler. `kotlin build` reads a toolchain
//! project and spawns `krusty` once per module. The compiler package does not depend on this one.

mod cli;

pub use cli::{locate_compiler, parse, CompilerSearch, Parsed, BUILD_HELP, ROOT_HELP};

use std::path::PathBuf;

use krusty_build::kotlin_toolchain::{execute, BuildCommand};

/// Run one `kotlin` invocation. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("kotlin: {message}");
            return 1;
        }
    };
    match parsed {
        Parsed::Help(text) => {
            println!("{text}");
            0
        }
        Parsed::Build(mut command) => execute_build(&mut command),
    }
}

fn execute_build(command: &mut BuildCommand) -> i32 {
    match locate_from_process() {
        Ok(compiler) => command.compiler = compiler,
        Err(message) => {
            eprintln!("kotlin: {message}");
            return 1;
        }
    }
    match std::env::current_dir() {
        Ok(directory) => command.directory = directory,
        Err(error) => {
            eprintln!("kotlin: cannot determine the working directory: {error}");
            return 1;
        }
    }
    match execute(command) {
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
            eprintln!("kotlin: {message}");
            1
        }
    }
}

fn locate_from_process() -> Result<PathBuf, String> {
    let override_path = std::env::var_os("KRUSTY_COMPILER").map(PathBuf::from);
    let executable = std::env::current_exe().map_err(|error| {
        format!("cannot locate this executable to find the krusty compiler: {error}")
    })?;
    let path_dirs = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    locate_compiler(&CompilerSearch {
        override_path,
        executable,
        path_dirs,
    })
}
