//! The host process: a JVM running the adapter on the provisioned compiler, which krusty drives over
//! its stdin and stdout. Its stderr is the user's, so what a plugin prints reaches them.

use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use crate::adapter;
use crate::environment::Environment;
use crate::protocol::{self, LoadedPlugin, Reply, Request, PROTOCOL};
use crate::provision::HostRuntime;

#[derive(Debug, PartialEq, Eq)]
pub enum HostError {
    /// The adapter did not compile.
    Build {
        detail: String,
    },
    Io {
        path: PathBuf,
        detail: String,
    },
    /// The JVM could not be started.
    Start {
        java: PathBuf,
        detail: String,
    },
    /// The host exited, or closed its end, before answering.
    Exited {
        status: String,
    },
    /// The host's output is not the protocol.
    Protocol {
        detail: String,
    },
    /// The host speaks another protocol version than this krusty.
    ProtocolVersion {
        host: u32,
    },
    /// The compiler the host loaded is not the build's exact Kotlin version.
    KotlinVersion {
        host: String,
        build: String,
    },
    /// The plugins refused the request, with kotlinc's own message: an unknown option, a missing
    /// required one, or a registrar that failed.
    Plugin {
        message: String,
    },
}

impl HostError {
    pub(crate) fn io(path: &Path, error: std::io::Error) -> Self {
        HostError::Io {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Build { detail } => {
                write!(f, "could not compile krusty's plugin host: {detail}")
            }
            HostError::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
            HostError::Start { java, detail } => write!(
                f,
                "could not start the plugin host with {}: {detail}",
                java.display()
            ),
            HostError::Exited { status } => write!(
                f,
                "the plugin host stopped before answering ({status}); its output above says why"
            ),
            HostError::Protocol { detail } => {
                write!(f, "the plugin host sent a malformed message: {detail}")
            }
            HostError::ProtocolVersion { host } => write!(
                f,
                "the plugin host speaks protocol {host}, but this krusty speaks {PROTOCOL}"
            ),
            HostError::KotlinVersion { host, build } => write!(
                f,
                "the plugin host loaded kotlinc {host}, but the build's Kotlin version is {build}"
            ),
            HostError::Plugin { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for HostError {}

/// A running host. Dropping it stops the process.
pub struct Host {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Host {
    /// Starts a host on `runtime`, compiling the adapter first if the cache has none for it, and
    /// checks that it loaded the build's exact Kotlin version.
    pub fn start(
        runtime: &HostRuntime,
        env: &Environment,
        report: &mut dyn Write,
    ) -> Result<Host, HostError> {
        let jar = adapter::jar(runtime, env, report)?;
        let classpath = std::iter::once(&jar).chain(&runtime.compiler_classpath);
        let classpath = std::env::join_paths(classpath).map_err(|error| HostError::Start {
            java: runtime.java.clone(),
            detail: format!("the classpath cannot be passed to java: {error}"),
        })?;
        let mut child = Command::new(&runtime.java)
            .arg("-cp")
            .arg(classpath)
            .arg(adapter::MAIN_CLASS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| HostError::Start {
                java: runtime.java.clone(),
                detail: error.to_string(),
            })?;
        let stdin = BufWriter::new(child.stdin.take().expect("a piped stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("a piped stdout"));
        let mut host = Host {
            child,
            stdin,
            stdout,
        };
        let hello = protocol::read_hello(&mut host.stdout).map_err(|error| host.failure(error))?;
        if hello.protocol != PROTOCOL {
            return Err(HostError::ProtocolVersion {
                host: hello.protocol,
            });
        }
        if hello.kotlin_version != runtime.kotlin_version {
            return Err(HostError::KotlinVersion {
                host: hello.kotlin_version,
                build: runtime.kotlin_version.clone(),
            });
        }
        Ok(host)
    }

    /// Loads a plugin classpath as kotlinc's `-Xplugin` does, with `options` the `-P` values
    /// (`plugin:<id>:<key>=<value>`), and returns each registrar it declares, in the order kotlinc's
    /// service loader finds them.
    pub fn load(
        &mut self,
        entries: &[String],
        options: &[String],
    ) -> Result<Vec<LoadedPlugin>, HostError> {
        let request = Request::Load { entries, options };
        match self.exchange(&request)? {
            Reply::Loaded(plugins) => Ok(plugins),
            Reply::Failed(message) => Err(HostError::Plugin { message }),
        }
    }

    fn exchange(&mut self, request: &Request) -> Result<Reply, HostError> {
        protocol::write_request(&mut self.stdin, request).map_err(|error| self.failure(error))?;
        protocol::read_reply(&mut self.stdout).map_err(|error| self.failure(error))
    }

    /// The error for a failed read or write: the host's exit when it is gone, else the bad message.
    fn failure(&mut self, error: std::io::Error) -> HostError {
        match error.kind() {
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe => {
                HostError::Exited {
                    status: describe(self.child.wait().ok()),
                }
            }
            _ => HostError::Protocol {
                detail: error.to_string(),
            },
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if protocol::write_request(&mut self.stdin, &Request::Shutdown).is_err() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn describe(status: Option<ExitStatus>) -> String {
    match status {
        Some(status) => status.to_string(),
        None => "its exit status is unknown".to_string(),
    }
}
