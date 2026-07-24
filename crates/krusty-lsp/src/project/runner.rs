//! Build-tool command execution.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const BUILD_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
}

impl Command {
    pub fn new(program: impl Into<PathBuf>, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            working_directory: working_directory.into(),
        }
    }

    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.args.push(argument.into());
        self
    }

    pub fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args
            .extend(arguments.into_iter().map(|argument| argument.into()));
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn succeeded(&self) -> bool {
        self.status == 0
    }
}

pub trait CommandRunner {
    fn run(&self, command: &Command) -> io::Result<CommandOutput>;
}

/// Runs commands as child processes.
#[derive(Debug, Default)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, command: &Command) -> io::Result<CommandOutput> {
        run_process(command, BUILD_TOOL_TIMEOUT)
    }
}

fn run_process(command: &Command, timeout: Duration) -> io::Result<CommandOutput> {
    let mut child = std::process::Command::new(&command.program)
        .args(&command.args)
        .current_dir(&command.working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let deadline = Instant::now() + timeout;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{} timed out", command.program.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))??;
    Ok(CommandOutput {
        status: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn read_all(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// How a build tool is invoked: its wrapper script names and the command to fall back to.
///
/// The wrapper always wins when it is present. It pins the build-tool version the project expects,
/// which is the version whose model the user actually gets from their own terminal — probing with a
/// different Gradle or Maven can produce a different classpath, or fail outright on a build that
/// uses newer syntax.
#[derive(Clone, Copy, Debug)]
pub struct Executable {
    /// Wrapper script relative to the build root, on Unix.
    pub wrapper: &'static str,
    /// Wrapper script relative to the build root, on Windows.
    pub windows_wrapper: &'static str,
    pub program: &'static str,
    pub windows_program: &'static str,
}

pub const GRADLE: Executable = Executable {
    wrapper: "gradlew",
    windows_wrapper: "gradlew.bat",
    program: "gradle",
    windows_program: "gradle.bat",
};

pub const MAVEN: Executable = Executable {
    wrapper: "mvnw",
    windows_wrapper: "mvnw.cmd",
    program: "mvn",
    windows_program: "mvn.cmd",
};

impl Executable {
    /// The project's wrapper when it exists, otherwise the tool on `PATH`.
    ///
    /// Windows needs the script name spelled out: `std::process::Command` appends `.exe` and does
    /// not consult `PATHEXT`, so a bare `gradle` would not be found there.
    pub fn resolve(&self, root: &Path) -> PathBuf {
        let (wrapper, program) = if cfg!(windows) {
            (self.windows_wrapper, self.windows_program)
        } else {
            (self.wrapper, self.program)
        };
        let candidate = root.join(wrapper);
        if candidate.is_file() {
            candidate
        } else {
            PathBuf::from(program)
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;

    use super::*;

    /// Replays canned output and records every command it was asked to run.
    #[derive(Default)]
    pub(crate) struct FakeRunner {
        responses: RefCell<Vec<io::Result<CommandOutput>>>,
        pub(crate) commands: RefCell<Vec<Command>>,
    }

    impl FakeRunner {
        pub(crate) fn new(responses: Vec<CommandOutput>) -> Self {
            Self {
                responses: RefCell::new(responses.into_iter().map(Ok).collect()),
                commands: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn stdout(output: &str) -> CommandOutput {
            CommandOutput {
                status: 0,
                stdout: output.to_string(),
                stderr: String::new(),
            }
        }

        pub(crate) fn failure(status: i32, stderr: &str) -> CommandOutput {
            CommandOutput {
                status,
                stdout: String::new(),
                stderr: stderr.to_string(),
            }
        }

        pub(crate) fn command(&self, index: usize) -> Command {
            self.commands.borrow()[index].clone()
        }

        pub(crate) fn command_count(&self) -> usize {
            self.commands.borrow().len()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, command: &Command) -> io::Result<CommandOutput> {
            self.commands.borrow_mut().push(command.clone());
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                return Ok(CommandOutput::default());
            }
            responses.remove(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeRunner;
    use super::*;
    use crate::project::testing::TempTree;

    #[test]
    fn a_project_wrapper_is_preferred_over_the_program_on_path() {
        let tree = TempTree::new("runner-wrapper");
        let platform_program = if cfg!(windows) {
            "gradle.bat"
        } else {
            "gradle"
        };
        assert_eq!(GRADLE.resolve(&tree.root), PathBuf::from(platform_program));

        let wrapper = if cfg!(windows) {
            "gradlew.bat"
        } else {
            "gradlew"
        };
        tree.write(wrapper, "#!/bin/sh\n");
        assert_eq!(GRADLE.resolve(&tree.root), tree.path(wrapper));
    }

    #[test]
    fn the_maven_wrapper_is_recognised_under_its_own_name() {
        let tree = TempTree::new("runner-mvnw");
        // A Gradle wrapper in the same tree must not be mistaken for Maven's.
        tree.write("gradlew", "#!/bin/sh\n");
        let platform_program = if cfg!(windows) { "mvn.cmd" } else { "mvn" };
        assert_eq!(MAVEN.resolve(&tree.root), PathBuf::from(platform_program));

        let wrapper = if cfg!(windows) { "mvnw.cmd" } else { "mvnw" };
        tree.write(wrapper, "#!/bin/sh\n");
        assert_eq!(MAVEN.resolve(&tree.root), tree.path(wrapper));
    }

    #[test]
    fn the_fake_runner_replays_responses_in_order_and_records_commands() {
        let runner = FakeRunner::new(vec![
            FakeRunner::stdout("first"),
            FakeRunner::failure(1, "boom"),
        ]);
        let command = Command::new("gradle", "/p").arg("--quiet");
        assert_eq!(runner.run(&command).unwrap().stdout, "first");
        let second = runner.run(&Command::new("mvn", "/p")).unwrap();
        assert!(!second.succeeded());
        assert_eq!(second.stderr, "boom");
        assert_eq!(runner.command(0), command);
        assert_eq!(runner.command_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_exceeds_the_deadline_is_terminated() {
        let command = Command::new("sh", "/").args(["-c", "sleep 2"]);
        let error = run_process(&command, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
