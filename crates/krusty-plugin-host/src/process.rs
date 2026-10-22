//! A host's JVM process, which krusty drives over its stdin and stdout. Its stderr is the user's, so
//! what a plugin or processor prints reaches them.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::host::HostError;
use crate::protocol;

/// How long a host may take to exit once asked to, or once its connection is lost, before it is
/// killed. A plugin can start threads that keep a JVM alive, and none of them may hang krusty.
pub(crate) const EXIT_GRACE: Duration = Duration::from_secs(5);

/// A running host process. Dropping it stops the process: it is sent its protocol's shutdown
/// request, and killed when it has not exited within the grace interval.
pub(crate) struct HostProcess {
    child: Child,
    /// `None` once closed, which tells the host to exit as a shutdown request does.
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: BufReader<ChildStdout>,
    grace: Duration,
    /// The tag of the protocol's shutdown request, a message with nothing else in it.
    shutdown: u8,
}

impl HostProcess {
    pub(crate) fn spawn(
        mut command: Command,
        grace: Duration,
        shutdown: u8,
    ) -> Result<HostProcess, HostError> {
        let program = PathBuf::from(command.get_program());
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| HostError::Start {
                java: program,
                detail: error.to_string(),
            })?;
        let stdin = Some(BufWriter::new(child.stdin.take().expect("a piped stdin")));
        let stdout = BufReader::new(child.stdout.take().expect("a piped stdout"));
        Ok(HostProcess {
            child,
            stdin,
            stdout,
            grace,
            shutdown,
        })
    }

    pub(crate) fn stdin(&mut self) -> &mut BufWriter<ChildStdin> {
        self.stdin.as_mut().expect("open until the host is dropped")
    }

    pub(crate) fn stdout(&mut self) -> &mut BufReader<ChildStdout> {
        &mut self.stdout
    }

    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    /// The error for a failed read or write: the host's exit when it is gone, else the bad message.
    pub(crate) fn failure(&mut self, error: std::io::Error) -> HostError {
        match error.kind() {
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe => {
                HostError::Exited {
                    status: self.reap(),
                }
            }
            _ => HostError::Protocol {
                detail: error.to_string(),
            },
        }
    }

    /// Waits up to the grace interval for the host to exit, then kills it, and always reaps it.
    /// Says how it ended.
    fn reap(&mut self) -> String {
        let deadline = Instant::now() + self.grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.to_string(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => break,
                Err(error) => return format!("its exit status is unknown: {error}"),
            }
        }
        let _ = self.child.kill();
        match self.child.wait() {
            Ok(_) => format!(
                "it did not exit within {} s and was stopped",
                self.grace.as_secs_f64()
            ),
            Err(error) => format!("it did not exit and could not be reaped: {error}"),
        }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        // A host that cannot take the request is ended the same way, after the same grace.
        if let Some(mut stdin) = self.stdin.take() {
            let _ = protocol::write_frame(&mut stdin, &[self.shutdown]);
        }
        self.reap();
    }
}
