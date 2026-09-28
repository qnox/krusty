//! Run Gradle or Maven and keep their stdout, stderr, and files.
//!
//! Build scripts, POMs, and Gradle module metadata stay inside those tools. This module only
//! starts a process and reports the text or classpath file the tool already produced.

use std::path::{Path, PathBuf};

/// One tool invocation. `program` is a wrapper path or a bare name resolved through `PATH`.
pub(super) struct ToolCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub directory: PathBuf,
}

pub(super) struct ToolOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub(super) trait ToolRunner {
    fn run(&self, command: &ToolCommand) -> Result<ToolOutput, String>;
}

pub(super) struct ProcessRunner;

impl ToolRunner for ProcessRunner {
    fn run(&self, command: &ToolCommand) -> Result<ToolOutput, String> {
        let output = std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.directory)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", command.program.display()))?;
        Ok(ToolOutput {
            status: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
pub(super) struct FnRunner<F>(pub F);

#[cfg(test)]
impl<F> ToolRunner for FnRunner<F>
where
    F: Fn(&ToolCommand) -> Result<ToolOutput, String>,
{
    fn run(&self, command: &ToolCommand) -> Result<ToolOutput, String> {
        (self.0)(command)
    }
}

pub(super) fn require_success(program: &Path, output: &ToolOutput) -> Result<(), String> {
    if output.status == 0 {
        Ok(())
    } else {
        Err(failure_message(program, output))
    }
}

pub(super) fn failure_message(program: &Path, output: &ToolOutput) -> String {
    let detail = first_line(&output.stderr)
        .or_else(|| first_line(&output.stdout))
        .unwrap_or_default();
    format!(
        "{} exited with status {}: {detail}",
        program.display(),
        output.status
    )
}

pub(super) fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub(super) fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_string)
}

/// `gradlew` / `mvnw` when the project ships one, otherwise the tool name on `PATH`.
pub(super) fn tool_program(
    root: &Path,
    unix_wrapper: &str,
    windows_wrapper: &str,
    plain: &str,
) -> PathBuf {
    let preferred: [&str; 2] = if cfg!(windows) {
        [windows_wrapper, unix_wrapper]
    } else {
        [unix_wrapper, windows_wrapper]
    };
    for name in preferred {
        let path = root.join(name);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from(plain)
}
